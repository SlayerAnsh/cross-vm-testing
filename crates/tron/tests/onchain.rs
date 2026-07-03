//! Live on-chain tests against the Nile testnet over the TronGrid HTTP API.
//!
//! Ignored by default (need network, and the write test needs a funded key). Run with:
//!   cargo test -p cross-vm-tron --test onchain -- --ignored --nocapture
//!
//! The `test` wallet is index 0 of `MNEMONIC_TEST` (`m/44'/195'/0'/0/0`, Tron coin type 195).
//! Fund that address on Nile (the write test prints it and fails fast if the balance is zero):
//! Nile faucet at <https://nileex.io/join/getJoinPage>.

use std::rc::Rc;
use std::time::Duration;

use alloy_primitives::keccak256;
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_macros::define_wallet_roster;
use cross_vm_tron::chains::NILE;
use cross_vm_tron::{Bytes, TronChain};

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
    };
}

const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");
const COUNTER_ARTIFACT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solidity/out/Counter.sol/Counter.json"
);

/// Read the Counter creation bytecode from the forge artifact at runtime (so this file compiles
/// even when the artifact has not been built; only the ignored test needs it).
fn counter_bytecode() -> Bytes {
    let raw = std::fs::read_to_string(COUNTER_ARTIFACT)
        .unwrap_or_else(|e| panic!("read {COUNTER_ARTIFACT}: {e} (run `make compile-solidity`)"));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse artifact json");
    let hexstr = v["bytecode"]["object"]
        .as_str()
        .expect("bytecode.object")
        .trim_start_matches("0x");
    Bytes::from(hex::decode(hexstr).expect("decode bytecode"))
}

fn selector(sig: &str) -> Bytes {
    Bytes::copy_from_slice(&keccak256(sig.as_bytes())[..4])
}

#[tokio::test]
#[ignore = "live: requires Nile RPC network access"]
async fn live_reads_on_nile() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let chain: TronChain = NILE.rpc(wallets).into();

    let height = chain.block_height().await;
    println!("nile block height: {height}");
    assert!(height > 0, "expected a positive Nile block height");
}

#[tokio::test]
#[ignore = "live: requires Nile RPC + funded MNEMONIC_TEST index 0 (coin 195)"]
async fn live_deploy_increment_count_on_nile() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: TronChain = NILE.rpc(wallets).into();

    let who = chain
        .wallet_address(ONCHAIN_WALLETS.test)
        .await
        .expect("derive test wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    println!("balance:     {balance} sun");
    assert!(balance > 0, "fund {who} on Nile first (balance is zero)");

    // Deploy Counter (EVM bytecode runs on the TVM), then increment and read back.
    let counter = chain
        .deploy_create(counter_bytecode(), Bytes::new(), ONCHAIN_WALLETS.test)
        .await
        .expect("deploy counter");
    println!("counter deployed at: {counter}");
    settle().await;

    // `call` polls for the receipt internally, so the incremented state is committed on return.
    let exec = chain
        .call(&counter, selector("increment()"), ONCHAIN_WALLETS.test)
        .await
        .expect("increment");
    println!("increment logs: {}", exec.logs.len());

    let out = chain
        .static_call(&counter, selector("count()"))
        .await
        .expect("count");
    let n = if out.len() >= 32 {
        u64::from_be_bytes(out[24..32].try_into().unwrap())
    } else {
        0
    };
    println!("count after one increment: {n}");
    assert_eq!(n, 1, "expected count == 1 after a single increment");
}

/// Nile blocks are ~3s; give a broadcast tx time to confirm before reading state.
async fn settle() {
    tokio::time::sleep(Duration::from_secs(8)).await;
}
