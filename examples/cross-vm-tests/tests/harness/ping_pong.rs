//! Property-testing harness for the cross-VM ping-pong relayer.
//!
//! Same subject as `cross_vm/ping_pong.rs`, lifted into a [`Harness`] so the fuzz / invariant /
//! endurance runners drive it. The two operations are `Ping { src, dst }` (send a packet) and
//! `Relay` (one relay *tick*: deliver the packets pending right now and stop). A packet thus
//! steps through its lifecycle across separate ticks — tick 1 receives (Receive + WriteAck),
//! tick 2 acknowledges (Ack) — instead of completing in a single atomic relay. The persisted
//! `PingPongWorld` holds the relayer's bookkeeping only: each chain's deployed account and port,
//! the shared [`BridgeLedger`], and a [`Bridge`] with every port registered. No live chain
//! handles live in the world; `apply` rebuilds a `PingPong` from `Ctx` + a stored account.
//!
//! Every transaction is parsed the same way — the user's ping and the relay's own
//! `receive_packet` / `acknowledge_packet` calls all go through `parse_packets` and land in the
//! ledger. Relay progress is *derived* from those events (a `SendPacket` is pending until a
//! `ReceivePacket` with its key appears; a `WriteAcknowledgement` until an `AcknowledgePacket`),
//! never tracked with side flags. There are no `on_after` hooks here: `apply` is async and owns
//! each response, so it records by parsing the returned `AppResponse`.
//!
//! Invariants checked at every step:
//! - `StatsMatchLedger`: each contract's on-chain `pings_sent` equals the `SendPacket`s the
//!   ledger attributes to its port, and `pongs_received` equals the `AcknowledgePacket`s. Both
//!   sides move in lockstep, so it holds before and after any tick.
//! - `NoOrphanEvents`: every `ReceivePacket`/`WriteAcknowledgement` has a preceding `SendPacket`
//!   and every `AcknowledgePacket` a preceding `WriteAcknowledgement`. A contract emitting a
//!   bogus lifecycle event (e.g. a `ReceivePacket` for a packet never sent) trips this.
//!
//! The endurance test drains all in-flight packets with repeated ticks after the run, then
//! asserts the ledger fully settled (every ping acknowledged, no orphans).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
#[cfg(feature = "endurance")]
use std::time::Duration;

use cross_vm_framework::prelude::*;

use crate::support::{
    fund_alice, parse_packets, test_wallets, Bridge, BridgeLedger, PacketKind, PingPong,
    PingPongSpec,
};

/// `(env label, chain id passed to the contract)`. CosmWasm ignores the chain id (it reads the
/// runtime block chain_id); EVM and Solana embed it in their port.
const CHAINS: [(&str, &str); 3] = [
    ("osmosis", "osmosis-1"),
    ("eth", "1"),
    ("solana", "solana-localnet"),
];

/// Persisted state for one run: per-chain deployed account and port, the shared ledger, and the
/// bridge (port registry + the same ledger). No chains or contract handles live here.
struct PingPongWorld {
    labels: Vec<String>,
    account: HashMap<String, Account>,
    port: HashMap<String, String>,
    ledger: Rc<RefCell<BridgeLedger>>,
    bridge: Bridge,
}

/// Rebuild a `PingPong` handle bound to the deployed instance on `label`. The chain is cloned
/// out of the env (shared state), so the handle drives the one live contract.
fn pp_handle(ctx: &Ctx, world: &PingPongWorld, label: &str) -> Result<PingPong, HarnessError> {
    let chain = ctx.chain(label)?;
    let account = world
        .account
        .get(label)
        .cloned()
        .ok_or_else(|| HarnessError::infra(format!("no ping-pong deployed on {label}")))?;
    Ok(PingPong::instance(chain, account))
}

/// Send a ping from `src` to `dst`'s port.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Ping {
    src: String,
    dst: String,
}

impl DynOp<Ctx, PingPongWorld> for Ping {
    fn kind(&self) -> &'static str {
        "ping"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut PingPongWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            let dst_port = w
                .port
                .get(&self.dst)
                .cloned()
                .ok_or_else(|| HarnessError::infra(format!("no port for {}", self.dst)))?;
            let pp = pp_handle(ctx, w, &self.src)?;
            let resp = pp
                .ping("alice", dst_port)
                .await
                .map_err(HarnessError::infra)?;
            let emitted = parse_packets(resp.kind(), resp.raw());
            w.ledger.borrow_mut().record_all(emitted);
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, PingPongWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// One relay tick: deliver the packets pending right now (receive, then acknowledge), and stop.
/// Events emitted this tick are left for the next relay.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Relay {}

impl DynOp<Ctx, PingPongWorld> for Relay {
    fn kind(&self) -> &'static str {
        "relay"
    }

    fn apply<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        w: &'a mut PingPongWorld,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>> {
        Box::pin(async move {
            // One tick only: relay the packets pending right now. The Receive/WriteAck/Ack this
            // emits are parsed and recorded (the bridge shares `w.ledger`) but left for the next
            // relay, so a packet steps through its stages across ticks.
            w.bridge
                .relay_tick(&mut ctx.env)
                .await
                .map_err(HarnessError::infra)?;
            Ok(Verdict::Accepted)
        })
    }

    fn clone_box(&self) -> Box<dyn DynOp<Ctx, PingPongWorld>> {
        Box::new(self.clone())
    }

    fn to_data(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("op data serializes")
    }
}

/// No causally-impossible packet event was ever recorded (e.g. a `ReceivePacket` for a packet
/// that was never sent). Catches a contract emitting a bogus lifecycle event.
#[derive(Clone, Debug)]
struct NoOrphanEvents;

impl DynInvariant<Ctx, PingPongWorld> for NoOrphanEvents {
    fn check<'a>(&'a self, _ctx: &'a mut Ctx, w: &'a PingPongWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            let orphans = w.ledger.borrow().orphans();
            if orphans.is_empty() {
                CheckOutcome::Held
            } else {
                CheckOutcome::violated(orphans.join("; "))
            }
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, PingPongWorld>> {
        Box::new(self.clone())
    }
}

/// Every contract's on-chain counters match what the ledger attributes to its port.
#[derive(Clone, Debug)]
struct StatsMatchLedger;

impl DynInvariant<Ctx, PingPongWorld> for StatsMatchLedger {
    fn check<'a>(&'a self, ctx: &'a mut Ctx, w: &'a PingPongWorld) -> OpFuture<'a, CheckOutcome> {
        Box::pin(async move {
            // Snapshot the ledger-derived expectations under a short borrow, then compare to
            // on-chain stats without holding the borrow across an await.
            let expected: Vec<(String, u64, u64)> = {
                let l = w.ledger.borrow();
                w.labels
                    .iter()
                    .map(|label| {
                        let port = &w.port[label];
                        let sent = l.count_for_source(PacketKind::Send, port) as u64;
                        let acked = l.count_for_source(PacketKind::Ack, port) as u64;
                        (label.clone(), sent, acked)
                    })
                    .collect()
            };
            for (label, sent, acked) in expected {
                let pp = match pp_handle(ctx, w, &label) {
                    Ok(p) => p,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                let stats = match pp.stats().await {
                    Ok(s) => s,
                    Err(e) => return CheckOutcome::violated(e.to_string()),
                };
                if stats.pings_sent != sent {
                    return CheckOutcome::violated(format!(
                        "{label}: pings_sent on-chain {} != ledger {sent}",
                        stats.pings_sent
                    ));
                }
                if stats.pongs_received != acked {
                    return CheckOutcome::violated(format!(
                        "{label}: pongs_received on-chain {} != ledger {acked}",
                        stats.pongs_received
                    ));
                }
            }
            CheckOutcome::Held
        })
    }

    fn clone_box(&self) -> Box<dyn DynInvariant<Ctx, PingPongWorld>> {
        Box::new(self.clone())
    }
}

fn gen_ping(rng: &mut Prng, w: &PingPongWorld) -> Box<dyn DynOp<Ctx, PingPongWorld>> {
    let src = w.labels[rng.index(w.labels.len())].clone();
    let dst = w.labels[rng.index(w.labels.len())].clone();
    Box::new(Ping { src, dst })
}

fn gen_relay(_rng: &mut Prng, _w: &PingPongWorld) -> Box<dyn DynOp<Ctx, PingPongWorld>> {
    Box::new(Relay {})
}

// Bias toward pings (relay roughly every third op) so packets accumulate and then drain.
fn weight_ping(_ctx: &Ctx, _w: &PingPongWorld) -> u32 {
    2
}

fn advance(ctx: &mut Ctx, blocks: u64) -> OpFuture<'_, Result<(), HarnessError>> {
    Box::pin(async move {
        ctx.advance_all(blocks).await;
        Ok(())
    })
}

/// Assemble the ping-pong relayer harness.
fn ping_pong_harness() -> OpSetHarness<Ctx, PingPongWorld> {
    OpSetHarness::new()
        .register(
            OpDef::new("ping", gen_ping, decode_json_op::<Ping, _, _>).with_weight(weight_ping),
        )
        .register(OpDef::new(
            "relay",
            gen_relay,
            decode_json_op::<Relay, _, _>,
        ))
        .invariant(Box::new(StatsMatchLedger))
        .invariant(Box::new(NoOrphanEvents))
        .with_advance(advance)
}

/// Build the live env (a ping-pong deployed on all three chains) and the primed world with the
/// bridge's ports registered. A free function the tests load into a runner with `r.setup(..)`.
async fn ping_pong_setup(_seed: u64) -> Result<(Ctx, PingPongWorld), HarnessError> {
    crate::support::init_tracing();
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("ping-pong-harness", wallets.clone());
    env.inject("osmosis", OSMOSIS.mock(wallets.clone()));
    env.inject("eth", ETHEREUM.mock(wallets.clone()));
    env.inject("solana", SOLANA_DEVNET.mock(wallets));
    let ctx = Ctx::new(env.start().await?);

    let ledger = Rc::new(RefCell::new(BridgeLedger::default()));
    let mut bridge = Bridge::new(ledger.clone());
    let mut account = HashMap::new();
    let mut port = HashMap::new();

    for (label, chain_id) in CHAINS {
        let mut chain = ctx.chain(label)?;
        fund_alice(&mut chain).await;

        let pp = PingPong::new(chain);
        pp.setup("alice", chain_id)
            .await
            .map_err(HarnessError::infra)?;
        let acct = pp
            .address()
            .ok_or_else(|| HarnessError::infra(format!("{label}: setup recorded no address")))?;
        let p = pp.port().await.map_err(HarnessError::infra)?;

        bridge.register(p.clone(), label, acct.clone(), "alice");
        account.insert(label.to_string(), acct);
        port.insert(label.to_string(), p);
    }

    Ok((
        ctx,
        PingPongWorld {
            labels: CHAINS.iter().map(|(l, _)| l.to_string()).collect(),
            account,
            port,
            ledger,
            bridge,
        },
    ))
}

// Scenario path (always runs under default `cargo test`): one ping then two relay ticks. The
// first tick receives (Receive + WriteAck), the second acknowledges (Ack) — the lifecycle steps
// across separate ticks, which is the point of single-tick relay.
#[tokio::test]
async fn ping_pong_round_trip_scenario() {
    let (ctx, world) = ping_pong_setup(0).await.expect("setup");
    let mut r = Runner::scenario(ping_pong_harness(), 0);
    r.setup(ctx, world);

    // After the ping + first tick: Send, Receive, WriteAck recorded, but no Ack yet.
    let report = r
        .run_scenario(vec![
            DynOperation(Box::new(Ping {
                src: "eth".to_string(),
                dst: "osmosis".to_string(),
            })),
            DynOperation(Box::new(Relay {})),
        ])
        .await;
    assert!(report.passed(), "{:?}", report.failure);
    {
        let l = r.world().ledger.borrow();
        assert_eq!(l.count(PacketKind::Receive), 1, "received after tick 1");
        assert_eq!(l.count(PacketKind::WriteAck), 1, "write-ack after tick 1");
        assert_eq!(l.count(PacketKind::Ack), 0, "no ack yet after tick 1");
        assert!(!l.fully_settled(), "write-ack still pending after tick 1");
    }

    // Second tick acknowledges; now the packet is fully settled.
    let report = r.run_case(DynOperation(Box::new(Relay {}))).await;
    assert!(report.passed(), "{:?}", report.failure);
    let l = r.world().ledger.borrow();
    assert_eq!(l.count(PacketKind::Ack), 1, "ack after tick 2");
    assert!(l.full_lifecycle(0), "full lifecycle for seq 0");
    assert!(l.fully_settled(), "settled after tick 2");
    assert!(
        l.orphans().is_empty(),
        "no orphan events: {:?}",
        l.orphans()
    );
}

#[cfg(feature = "invariant")]
#[invariant_runner(harness = ping_pong_harness(), seed = 7)]
async fn ping_pong_invariant_mode(
    #[runner] mut r: InvariantRunner<OpSetHarness<Ctx, PingPongWorld>>,
) {
    let (ctx, world) = ping_pong_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(30, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
    assert_eq!(report.steps, 30);
}

#[cfg(feature = "endurance")]
#[endurance_runner(harness = ping_pong_harness(), seed = 1)]
async fn ping_pong_endurance_mode(
    #[runner] mut r: EnduranceRunner<OpSetHarness<Ctx, PingPongWorld>>,
) {
    let (ctx, world) = ping_pong_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r
        .run(
            EnduranceConfig::new(Duration::from_millis(5000))
                .check_every(5)
                .advance_blocks(1, 1),
        )
        .await;
    assert!(report.passed(), "{:?}", report.failure);
    assert!(report.steps > 0, "endurance ran zero steps");

    // The run leaves packets mid-flight (single-tick relays during the loop). Drain them: relay
    // tick after tick until nothing is pending, then assert the whole world settled cleanly.
    let (mut ctx, world) = r.into_parts();
    let mut bridge = world.bridge;
    bridge.relay(&mut ctx.env).await.expect("final drain");

    let l = world.ledger.borrow();
    assert!(l.fully_settled(), "packets still in flight after drain");
    assert_eq!(
        l.count(PacketKind::Send),
        l.count(PacketKind::Ack),
        "every ping eventually acknowledged"
    );
    assert!(l.orphans().is_empty(), "orphan events: {:?}", l.orphans());
}

// Fans out into `ping_pong_fuzz_case_0` .. `ping_pong_fuzz_case_7`, each its own libtest entry.
#[cfg(feature = "fuzz")]
#[fuzz_runner(harness = ping_pong_harness(), seed = 7, cases = 8)]
async fn ping_pong_fuzz(#[runner] mut r: FuzzRunner<OpSetHarness<Ctx, PingPongWorld>>) {
    let (ctx, world) = ping_pong_setup(r.seed()).await.expect("setup");
    r.setup(ctx, world);
    let report = r.run(25, None, 1).await;
    assert!(report.passed(), "{:?}", report.failure);
}
