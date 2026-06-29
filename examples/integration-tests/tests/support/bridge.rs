//! A minimal IBC-style relayer for the cross-VM ping-pong contracts.
//!
//! Two halves:
//! - [`BridgeLedger`] + [`parse_packets`]: the *observation* side. A contract's `on_after`
//!   hook is synchronous and read-only (it cannot touch `env` or `.await`), so it only
//!   *records* packet-lifecycle events it parses out of the uniform [`RawResponse`] into a
//!   shared `Rc<RefCell<BridgeLedger>>`. The same parser serves the [`Bridge`] when it reads
//!   the events its own relay calls emit.
//! - [`Bridge`]: the *action* side. It owns a port registry (`port -> Endpoint`) and, given
//!   `&mut MultiChainEnv<Running>`, relays each recorded `SendPacket` to its destination
//!   contract (`receive_packet`) and the resulting `WriteAcknowledgement` back to the source
//!   (`acknowledge_packet`). A port string `"{chain_id}.{address}"` carries both the chain
//!   (resolved via `env_label` -> `env.chain`) and the contract (the [`Account`]).
//!
//! Borrow discipline: the ledger `RefCell` is borrowed by the recording hook *and* by the
//! relay loop. The relay therefore never holds a ledger borrow across an `.await` or a
//! contract call (whose hook would re-borrow it) — each step collects work under a short
//! borrow, drops it, performs the async calls, then re-borrows briefly to set flags. Marking
//! by index is safe because records are only ever appended.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use base64::Engine;
use cross_vm_framework::prelude::*;

use super::ping_pong::{evm_pp, PingPong, PingPongSpec};

// Anchor event discriminators: `sha256("event:<Name>")[..8]` (the `event:` prefix, vs
// `global:` for instructions). Solana `emit!` writes these in `Program data: <base64>` lines.
const EV_SEND_PACKET: [u8; 8] = [72, 116, 140, 248, 208, 166, 244, 59];
const EV_RECEIVE_PACKET: [u8; 8] = [106, 174, 101, 232, 247, 111, 118, 82];
const EV_WRITE_ACK: [u8; 8] = [176, 64, 95, 0, 231, 173, 112, 237];
const EV_ACK_PACKET: [u8; 8] = [138, 45, 150, 88, 145, 107, 87, 92];

/// Which stage of the packet lifecycle a [`PacketEvent`] represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind {
    /// `ping` emitted it on the source chain.
    Send,
    /// `receive_packet` emitted it on the destination chain.
    Receive,
    /// `receive_packet` also emitted this acknowledgement on the destination chain.
    WriteAck,
    /// `acknowledge_packet` emitted it back on the source chain.
    Ack,
}

/// One packet-lifecycle event, parsed VM-agnostically from a contract execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketEvent {
    pub kind: PacketKind,
    pub source_port: String,
    pub destination_port: String,
    pub sequence: u64,
    /// Present on `Send` / `WriteAck` (the packet payload, `"ping"`).
    pub msg: Option<String>,
    /// Present on `WriteAck` (the acknowledgement, `"pong"`).
    pub ack: Option<String>,
}

/// A packet's identity, shared by all four of its lifecycle events.
type PacketKey<'a> = (&'a str, &'a str, u64);

fn key(e: &PacketEvent) -> PacketKey<'_> {
    (&e.source_port, &e.destination_port, e.sequence)
}

/// The append-only log of every packet event the relayer has observed — from user pings *and*
/// from the relay's own `receive_packet` / `acknowledge_packet` calls, all parsed the same way.
///
/// Relay state is *derived* from the events, never tracked with side flags: a `SendPacket` is
/// pending relay until a `ReceivePacket` with the same key appears; a `WriteAcknowledgement` is
/// pending ack until an `AcknowledgePacket` with the same key appears. Because the ledger is
/// fed by the same parser for every transaction, a stray event (e.g. a `ReceivePacket` for a
/// packet that was never sent) shows up as an orphan and trips [`orphans`](Self::orphans).
#[derive(Default)]
pub struct BridgeLedger {
    pub events: Vec<PacketEvent>,
}

impl BridgeLedger {
    /// Append the parsed events of one execution.
    pub fn record_all(&mut self, events: Vec<PacketEvent>) {
        self.events.extend(events);
    }

    /// How many recorded events are of `kind`.
    pub fn count(&self, kind: PacketKind) -> usize {
        self.events.iter().filter(|e| e.kind == kind).count()
    }

    /// How many recorded events are of `kind` and originate from `source_port`.
    pub fn count_for_source(&self, kind: PacketKind, source_port: &str) -> usize {
        self.events
            .iter()
            .filter(|e| e.kind == kind && e.source_port == source_port)
            .count()
    }

    /// The first recorded event matching `kind` and `sequence`, if any.
    pub fn find(&self, kind: PacketKind, sequence: u64) -> Option<&PacketEvent> {
        self.events
            .iter()
            .find(|e| e.kind == kind && e.sequence == sequence)
    }

    /// Whether an event of `kind` with this packet key has been recorded.
    fn has(&self, kind: PacketKind, k: PacketKey<'_>) -> bool {
        self.events.iter().any(|e| e.kind == kind && key(e) == k)
    }

    /// `SendPacket`s with no matching `ReceivePacket` yet — the packets a relay tick must receive.
    pub fn pending_receives(&self) -> Vec<PacketEvent> {
        self.events
            .iter()
            .filter(|e| e.kind == PacketKind::Send && !self.has(PacketKind::Receive, key(e)))
            .cloned()
            .collect()
    }

    /// `WriteAcknowledgement`s with no matching `AcknowledgePacket` yet — the acks a relay tick
    /// must send back to the source.
    pub fn pending_acks(&self) -> Vec<PacketEvent> {
        self.events
            .iter()
            .filter(|e| e.kind == PacketKind::WriteAck && !self.has(PacketKind::Ack, key(e)))
            .cloned()
            .collect()
    }

    /// Whether every packet lifecycle stage has settled (no pending receives or acks).
    pub fn fully_settled(&self) -> bool {
        self.pending_receives().is_empty() && self.pending_acks().is_empty()
    }

    /// Whether all four lifecycle stages were recorded for `sequence`.
    pub fn full_lifecycle(&self, sequence: u64) -> bool {
        [
            PacketKind::Send,
            PacketKind::Receive,
            PacketKind::WriteAck,
            PacketKind::Ack,
        ]
        .iter()
        .all(|k| self.find(*k, sequence).is_some())
    }

    /// Causally-impossible events: a `ReceivePacket`/`WriteAcknowledgement` without its
    /// antecedent `SendPacket`, or an `AcknowledgePacket` without its `WriteAcknowledgement`.
    /// A correct relayer over correct contracts produces none; a stray emission shows up here.
    pub fn orphans(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.events {
            let antecedent = match e.kind {
                PacketKind::Send => continue,
                PacketKind::Receive | PacketKind::WriteAck => PacketKind::Send,
                PacketKind::Ack => PacketKind::WriteAck,
            };
            if !self.has(antecedent, key(e)) {
                out.push(format!(
                    "orphan {:?} {}->{} seq {} (no preceding {:?})",
                    e.kind, e.source_port, e.destination_port, e.sequence, antecedent
                ));
            }
        }
        out
    }
}

/// An `on_after` callback that records every packet event of an execution into `ledger`.
/// Attach to a wrapper with `wrapper.on_after(record_hook(ledger.clone()))`.
pub fn record_hook(
    ledger: Rc<RefCell<BridgeLedger>>,
) -> impl FnMut(&HookContext) -> Result<(), CrossVmError> {
    move |ctx| {
        let events = parse_packets(ctx.kind(), ctx.raw());
        ledger.borrow_mut().record_all(events);
        Ok(())
    }
}

/// Where and how to call the contract that owns a given port.
///
/// A port `"{chain_id}.{address}"` is the public identity; the [`Endpoint`] is the private
/// "how to reach it": which env chain (`env_label`), which on-chain [`Account`], and which
/// wallet signs. For Solana the `account` is the per-user PDA and `signer` is that PDA's
/// owner; the source endpoint's `signer` MUST be the wallet that sent the `ping`, since
/// `acknowledge_packet` credits `pongs_received` on the signer's PDA.
#[derive(Clone)]
pub struct Endpoint {
    pub env_label: String,
    pub account: Account,
    pub signer: String,
}

/// The relayer: a shared ledger plus a `port -> Endpoint` registry.
pub struct Bridge {
    ledger: Rc<RefCell<BridgeLedger>>,
    ports: HashMap<String, Endpoint>,
}

impl Bridge {
    /// Build a bridge over a shared ledger (the same `Rc` the recording hooks write to).
    pub fn new(ledger: Rc<RefCell<BridgeLedger>>) -> Self {
        Self {
            ledger,
            ports: HashMap::new(),
        }
    }

    /// Register the contract reachable at `port` (chain via `env_label`, contract via
    /// `account`, signed by `signer`).
    pub fn register(
        &mut self,
        port: impl Into<String>,
        env_label: &str,
        account: Account,
        signer: &str,
    ) {
        self.ports.insert(
            port.into(),
            Endpoint {
                env_label: env_label.to_string(),
                account,
                signer: signer.to_string(),
            },
        );
    }

    fn endpoint(&self, port: &str) -> Result<Endpoint, CrossVmError> {
        self.ports
            .get(port)
            .cloned()
            .ok_or_else(|| CrossVmError::Other {
                kind: ChainKind::CosmWasm,
                reason: format!("no endpoint registered for port {port}"),
            })
    }

    /// One relay tick: process the packets pending *right now* (snapshot taken at entry) and
    /// stop. Each pending `SendPacket` is delivered with `receive_packet` (emitting
    /// `ReceivePacket` + `WriteAcknowledgement`) and each pending `WriteAcknowledgement` is
    /// returned with `acknowledge_packet` (emitting `AcknowledgePacket`). The events those calls
    /// emit are recorded but **not** processed in this tick — they become the next tick's work,
    /// so a packet visibly steps Send -> Receive/WriteAck -> Ack across separate ticks. Returns
    /// whether anything was relayed.
    ///
    /// The `receive_packet` payload is the `msg` carried by the observed `SendPacket`, not a
    /// hardcoded `"ping"`, so the relayer forwards exactly what it saw.
    pub async fn relay_tick(
        &mut self,
        env: &mut MultiChainEnv<Running>,
    ) -> Result<bool, CrossVmError> {
        // Snapshot both work sets at entry, before this tick emits anything.
        let to_receive = self.ledger.borrow().pending_receives();
        let to_ack = self.ledger.borrow().pending_acks();
        let mut progressed = false;

        for ev in to_receive {
            let ep = self.endpoint(&ev.destination_port)?;
            let resp = self
                .instance(env, &ep)?
                .receive_packet(
                    &ep.signer,
                    ev.source_port.clone(),
                    ev.destination_port.clone(),
                    ev.sequence,
                    ev.msg.clone().unwrap_or_default(),
                )
                .await?;
            let emitted = parse_packets(resp.kind(), resp.raw());
            self.ledger.borrow_mut().record_all(emitted);
            progressed = true;
        }

        for ev in to_ack {
            let ep = self.endpoint(&ev.source_port)?;
            let resp = self
                .instance(env, &ep)?
                .acknowledge_packet(
                    &ep.signer,
                    ev.source_port.clone(),
                    ev.destination_port.clone(),
                    ev.sequence,
                )
                .await?;
            let emitted = parse_packets(resp.kind(), resp.raw());
            self.ledger.borrow_mut().record_all(emitted);
            progressed = true;
        }

        Ok(progressed)
    }

    /// Relay tick after tick until nothing is pending — drains every in-flight packet to a
    /// settled `AcknowledgePacket`. Used by the plain test and the end-of-run drain.
    pub async fn relay(&mut self, env: &mut MultiChainEnv<Running>) -> Result<(), CrossVmError> {
        while self.relay_tick(env).await? {}
        Ok(())
    }

    /// Build a contract handle for an endpoint (a fresh, hookless instance over the shared chain).
    fn instance(
        &self,
        env: &MultiChainEnv<Running>,
        ep: &Endpoint,
    ) -> Result<PingPong, CrossVmError> {
        let chain = env.chain(&ep.env_label).map_err(|e| CrossVmError::Other {
            kind: ep.account.kind(),
            reason: e.to_string(),
        })?;
        Ok(PingPong::instance(chain, ep.account.clone()))
    }
}

/// Parse the packet-lifecycle events out of one execution's [`RawResponse`], VM-agnostically.
/// Shared by the recording `on_after` hook (via `HookContext::raw()`) and the [`Bridge`] (via
/// `AppResponse::raw()`); both expose `&RawResponse`.
pub fn parse_packets(kind: ChainKind, raw: &RawResponse) -> Vec<PacketEvent> {
    match kind {
        ChainKind::CosmWasm => parse_cosmwasm(raw),
        ChainKind::Evm => parse_evm(raw),
        ChainKind::Svm => parse_solana(raw),
    }
}

fn parse_cosmwasm(raw: &RawResponse) -> Vec<PacketEvent> {
    let Ok(events) = raw.cosmwasm_events() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ev in events {
        // cw-multi-test prepends `wasm-` to contract-emitted custom events.
        let ty = ev.ty.strip_prefix("wasm-").unwrap_or(ev.ty.as_str());
        let kind = match ty {
            "SendPacket" => PacketKind::Send,
            "ReceivePacket" => PacketKind::Receive,
            "WriteAcknowledgement" => PacketKind::WriteAck,
            "AcknowledgePacket" => PacketKind::Ack,
            _ => continue,
        };
        let attr = |key: &str| -> Option<String> {
            ev.attributes
                .iter()
                .find(|a| a.key == key)
                .map(|a| a.value.clone())
        };
        let sequence = attr("packet_sequence")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        out.push(PacketEvent {
            kind,
            source_port: attr("source_port").unwrap_or_default(),
            destination_port: attr("destination_port").unwrap_or_default(),
            sequence,
            msg: attr("msg"),
            ack: attr("ack"),
        });
    }
    out
}

fn parse_evm(raw: &RawResponse) -> Vec<PacketEvent> {
    use alloy::sol_types::SolEvent;

    let Ok(logs) = raw.evm_logs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for log in logs {
        if let Ok(d) = evm_pp::PingPong::SendPacket::decode_log(log) {
            let e = d.data;
            out.push(PacketEvent {
                kind: PacketKind::Send,
                source_port: e.source_port,
                destination_port: e.destination_port,
                sequence: e.packet_sequence,
                msg: Some(bytes_to_string(&e.msg)),
                ack: None,
            });
        } else if let Ok(d) = evm_pp::PingPong::ReceivePacket::decode_log(log) {
            let e = d.data;
            out.push(PacketEvent {
                kind: PacketKind::Receive,
                source_port: e.source_port,
                destination_port: e.destination_port,
                sequence: e.packet_sequence,
                msg: None,
                ack: None,
            });
        } else if let Ok(d) = evm_pp::PingPong::WriteAcknowledgement::decode_log(log) {
            let e = d.data;
            out.push(PacketEvent {
                kind: PacketKind::WriteAck,
                source_port: e.source_port,
                destination_port: e.destination_port,
                sequence: e.packet_sequence,
                msg: Some(bytes_to_string(&e.msg)),
                ack: Some(bytes_to_string(&e.ack)),
            });
        } else if let Ok(d) = evm_pp::PingPong::AcknowledgePacket::decode_log(log) {
            let e = d.data;
            out.push(PacketEvent {
                kind: PacketKind::Ack,
                source_port: e.source_port,
                destination_port: e.destination_port,
                sequence: e.packet_sequence,
                msg: None,
                ack: None,
            });
        }
    }
    out
}

fn parse_solana(raw: &RawResponse) -> Vec<PacketEvent> {
    let Ok(logs) = raw.solana_logs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in logs {
        let Some(b64) = line.strip_prefix("Program data: ") else {
            continue;
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
            continue;
        };
        if bytes.len() < 8 {
            continue;
        }
        let (disc, body) = bytes.split_at(8);
        let mut o = 0usize;
        let event = if disc == EV_SEND_PACKET {
            let source_port = read_string(body, &mut o);
            let destination_port = read_string(body, &mut o);
            let sequence = read_u64(body, &mut o);
            let msg = read_string(body, &mut o);
            PacketEvent {
                kind: PacketKind::Send,
                source_port,
                destination_port,
                sequence,
                msg: Some(msg),
                ack: None,
            }
        } else if disc == EV_RECEIVE_PACKET {
            let source_port = read_string(body, &mut o);
            let destination_port = read_string(body, &mut o);
            let sequence = read_u64(body, &mut o);
            PacketEvent {
                kind: PacketKind::Receive,
                source_port,
                destination_port,
                sequence,
                msg: None,
                ack: None,
            }
        } else if disc == EV_WRITE_ACK {
            let source_port = read_string(body, &mut o);
            let destination_port = read_string(body, &mut o);
            let sequence = read_u64(body, &mut o);
            let msg = read_string(body, &mut o);
            let ack = read_string(body, &mut o);
            PacketEvent {
                kind: PacketKind::WriteAck,
                source_port,
                destination_port,
                sequence,
                msg: Some(msg),
                ack: Some(ack),
            }
        } else if disc == EV_ACK_PACKET {
            let source_port = read_string(body, &mut o);
            let destination_port = read_string(body, &mut o);
            let sequence = read_u64(body, &mut o);
            PacketEvent {
                kind: PacketKind::Ack,
                source_port,
                destination_port,
                sequence,
                msg: None,
                ack: None,
            }
        } else {
            continue;
        };
        out.push(event);
    }
    out
}

fn bytes_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// ----- Minimal borsh primitives, shared with the Solana wrapper hooks. -----

/// Read a borsh `u32` length-prefixed UTF-8 string, advancing `o`. Returns `""` on a short
/// or invalid buffer (test code; malformed input means an empty field, not a panic).
pub(crate) fn read_string(b: &[u8], o: &mut usize) -> String {
    let len = read_u32(b, o) as usize;
    let Some(slice) = b.get(*o..*o + len) else {
        return String::new();
    };
    *o += len;
    String::from_utf8_lossy(slice).into_owned()
}

pub(crate) fn read_u32(b: &[u8], o: &mut usize) -> u32 {
    let Some(slice) = b.get(*o..*o + 4) else {
        return 0;
    };
    *o += 4;
    u32::from_le_bytes(slice.try_into().unwrap())
}

pub(crate) fn read_u64(b: &[u8], o: &mut usize) -> u64 {
    let Some(slice) = b.get(*o..*o + 8) else {
        return 0;
    };
    *o += 8;
    u64::from_le_bytes(slice.try_into().unwrap())
}

/// Append a borsh `u32`-length-prefixed string to `out`.
pub(crate) fn write_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Append a borsh `u64` to `out`.
pub(crate) fn write_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
