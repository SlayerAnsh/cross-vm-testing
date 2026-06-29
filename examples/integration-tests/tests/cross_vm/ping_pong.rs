//! Cross-VM ping-pong over a relayer.
//!
//! Deploy a `PingPong` on a CosmWasm, an EVM, and a Solana chain in one `MultiChainEnv`. Each
//! deployed handle carries an `on_after` hook that parses emitted packet events into a shared
//! [`BridgeLedger`]. A user `ping` on the source records a `SendPacket`; the [`Bridge`] then
//! relays it with access to `env` — `receive_packet` on the destination (recording
//! `ReceivePacket` + `WriteAcknowledgement`) and `acknowledge_packet` back on the source
//! (recording `AcknowledgePacket`). We assert the full lifecycle landed in the ledger and the
//! contracts' counters moved, for every ordered VM pair.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cross_vm_framework::prelude::*;
use rstest::rstest;

use crate::support::{
    fund_alice, test_wallets, Bridge, BridgeLedger, PacketKind, PingPong, PingPongSpec,
};

/// `(env label, chain id passed to the contract)`. CosmWasm ignores the chain id (it reads
/// the runtime block chain_id); EVM and Solana embed it in their port.
const CHAINS: [(&str, &str); 3] = [
    ("osmosis", "osmosis-1"),
    ("eth", "1"),
    ("solana", "solana-localnet"),
];

/// Build a started 3-chain env, deploy a hooked `PingPong` on each, and a [`Bridge`] with every
/// port registered. Returns the env, the wrappers keyed by env label, the bridge, and the
/// shared ledger.
async fn setup_world() -> (
    MultiChainEnv<Running>,
    HashMap<String, PingPong>,
    Bridge,
    Rc<RefCell<BridgeLedger>>,
) {
    let wallets = test_wallets();
    let mut env = MultiChainEnv::new("ping-pong", wallets.clone());
    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
    env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets.clone())));
    env.inject(
        "solana",
        AnyChain::from(SOLANA_DEVNET.mock(wallets.clone())),
    );
    let env = env.start().await.expect("start");

    let ledger = Rc::new(RefCell::new(BridgeLedger::default()));
    let mut bridge = Bridge::new(ledger.clone());
    let mut wrappers = HashMap::new();

    for (label, chain_id) in CHAINS {
        let mut chain = env.chain(label).expect("chain");
        fund_alice(&mut chain).await;

        let pp = PingPong::new(chain);
        pp.on_after(crate::support::record_hook(ledger.clone()));
        pp.setup("alice", chain_id).await.expect("setup");

        let port = pp.port().await.expect("port");
        let account = pp.address().expect("deployed address");
        bridge.register(port, label, account, "alice");
        wrappers.insert(label.to_string(), pp);
    }

    (env, wrappers, bridge, ledger)
}

/// Every ordered VM pair: each VM acts as both source and destination across the cases.
#[rstest]
#[case("eth", "osmosis")]
#[case("osmosis", "solana")]
#[case("solana", "eth")]
#[case("osmosis", "eth")]
#[case("eth", "solana")]
#[case("solana", "osmosis")]
#[tokio::test]
async fn ping_pong_relays_across_vms(#[case] src: &str, #[case] dst: &str) {
    let (mut env, pps, mut bridge, ledger) = setup_world().await;

    let dst_port = pps[dst].port().await.expect("dst port");
    pps[src].ping("alice", dst_port).await.expect("ping");

    // The user's ping recorded exactly one SendPacket; nothing relayed yet.
    assert_eq!(ledger.borrow().count(PacketKind::Send), 1);
    assert_eq!(ledger.borrow().count(PacketKind::Receive), 0);

    bridge.relay(&mut env).await.expect("relay");

    {
        let l = ledger.borrow();
        assert_eq!(l.count(PacketKind::Send), 1, "send");
        assert_eq!(l.count(PacketKind::Receive), 1, "receive");
        assert_eq!(l.count(PacketKind::WriteAck), 1, "write_ack");
        assert_eq!(l.count(PacketKind::Ack), 1, "ack");
        assert!(l.full_lifecycle(0), "full lifecycle for seq 0");

        // The acknowledgement landed on the destination's port and came back to the source's.
        let send = l.find(PacketKind::Send, 0).expect("send event");
        assert_eq!(send.msg.as_deref(), Some("ping"));
        let write_ack = l.find(PacketKind::WriteAck, 0).expect("write ack event");
        assert_eq!(write_ack.ack.as_deref(), Some("pong"));
    }

    // Source sent one ping and received one pong; destination only received.
    let src_stats = pps[src].stats().await.expect("src stats");
    assert_eq!(src_stats.pings_sent, 1, "src pings_sent");
    assert_eq!(src_stats.pongs_received, 1, "src pongs_received");
    assert_eq!(
        pps[dst].stats().await.expect("dst stats").pings_sent,
        0,
        "dst pings_sent"
    );
}

/// A focused walk-through of the heterogeneous EVM -> CosmWasm case, asserting the recorded
/// ports thread through correctly.
#[tokio::test]
async fn evm_to_cosmwasm_ports_thread_through() {
    let (mut env, pps, mut bridge, ledger) = setup_world().await;

    let eth_port = pps["eth"].port().await.expect("eth port");
    let osmo_port = pps["osmosis"].port().await.expect("osmo port");
    assert!(eth_port.starts_with("1."), "evm port: {eth_port}");

    pps["eth"]
        .ping("alice", osmo_port.clone())
        .await
        .expect("ping");
    bridge.relay(&mut env).await.expect("relay");

    let l = ledger.borrow();
    let send = l.find(PacketKind::Send, 0).expect("send");
    assert_eq!(send.source_port, eth_port);
    assert_eq!(send.destination_port, osmo_port);
    let ack = l.find(PacketKind::Ack, 0).expect("ack");
    assert_eq!(ack.source_port, eth_port);
    assert_eq!(ack.destination_port, osmo_port);
}
