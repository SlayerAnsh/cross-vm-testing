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
use cross_vm_tron::{Bytes, TronChain, TronCompute, TronEnergyPolicy, TronLimit};

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
        // Transfer recipient. java-tron rejects a transfer to the sender's own address, so the
        // live transfer test needs a second address; index 1 of the same mnemonic needs no
        // funding of its own (it only receives).
        recipient: env_mnemonic("MNEMONIC_TEST") @ 1,
    };
}

/// The energy-payment policy every deploy here carries: the caller pays all of a call's energy,
/// so the contract owner's ceiling never binds and these tests never spend the deployer's staked
/// energy on someone else's call.
const CALLER_PAYS: TronEnergyPolicy = TronEnergyPolicy {
    consume_user_resource_percent: 100,
    origin_energy_limit: 0,
};

/// A fee ceiling of 1000 TRX, in sun: what this crate used to hardcode for every write, kept here
/// as the explicit-cap case so the `Fee` path stays exercised on a live chain alongside
/// `Estimated`.
const GENEROUS_FEE: TronLimit = TronLimit::Fee(1_000_000_000);

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

    // Forecast the deploy before running it. `triggerconstantcontract` runs the initcode at the
    // node without broadcasting, so this costs nothing and changes nothing on chain.
    let forecast = chain
        .estimate_deploy_create(counter_bytecode(), Bytes::new(), ONCHAIN_WALLETS.test)
        .await
        .expect("estimate deploy");
    println!("deploy forecast:     {forecast:?}");
    let deploy_forecast = energy(forecast.compute);
    assert!(
        forecast.bandwidth > 0,
        "a transaction is billed for its bytes"
    );

    // Deploy Counter (EVM bytecode runs on the TVM), then increment and read back.
    // `Estimated` re-runs that forecast, scales it by the chain's `gas_adjustment`, and prices the
    // energy into a sun `fee_limit` at the node's current energy price. A deploy that fits the
    // forecast therefore fits the cap; one that does not fails as OUT_OF_ENERGY, which is the
    // point of a cap.
    let deploy = chain
        .deploy_create(
            counter_bytecode(),
            Bytes::new(),
            ONCHAIN_WALLETS.test,
            TronLimit::Estimated,
            CALLER_PAYS,
        )
        .await
        .expect("deploy counter");
    let counter = deploy.address;
    println!("counter deployed at: {counter} (txID {})", deploy.tx_hash);
    println!("deploy resources:    {:?}", deploy.resources);
    assert_eq!(deploy.tx_hash.len(), 64, "expected a 32-byte txID in hex");
    // A live chain meters energy (the mock, being revm, meters gas: a different quantity).
    assert!(
        matches!(deploy.resources.compute, TronCompute::Energy(e) if e > 0),
        "expected the node's energy_usage_total, got {:?}",
        deploy.resources.compute
    );
    assert!(
        deploy.resources.fee.is_some_and(|f| f > 0),
        "a deploy burns TRX"
    );
    // The forecast and the receipt are the same type, denominated in the same unit, so they compare
    // directly. They are not required to be equal (the node estimates against a slightly older
    // state), only to be the same order of magnitude: a forecast off by more than 2x is wired to
    // the wrong quantity, which is the failure this test exists to catch.
    assert_close(deploy_forecast, energy(deploy.resources.compute), "deploy");
    settle().await;

    // Forecast the increment, then run it. Estimating is not a transaction, so the count below is
    // still expected to be exactly 1 afterwards.
    let forecast = chain
        .estimate_call(&counter, selector("increment()"), ONCHAIN_WALLETS.test)
        .await
        .expect("estimate increment");
    println!("increment forecast: {forecast:?}");
    let call_forecast = energy(forecast.compute);

    // `call` polls for the receipt internally, so the incremented state is committed on return.
    let exec = chain
        .call(
            &counter,
            selector("increment()"),
            ONCHAIN_WALLETS.test,
            TronLimit::Estimated,
        )
        .await
        .expect("increment");
    println!("increment logs: {}", exec.logs.len());
    println!("increment txID: {}", exec.tx_hash);
    println!("increment resources: {:?}", exec.resources);
    assert_eq!(
        exec.tx_hash.len(),
        64,
        "a broadcast call reports the node's 32-byte txID in hex"
    );
    assert!(
        matches!(exec.resources.compute, TronCompute::Energy(e) if e > 0),
        "expected the node's energy_usage_total, got {:?}",
        exec.resources.compute
    );
    assert_close(call_forecast, energy(exec.resources.compute), "increment");

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

#[tokio::test]
#[ignore = "live: requires Nile RPC + funded MNEMONIC_TEST index 0 (coin 195)"]
async fn live_get_storage_at_on_nile() {
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

    // Counter's `uint256 public count` is the sole state variable, so it occupies storage slot 0.
    let counter = chain
        .deploy_create(
            counter_bytecode(),
            Bytes::new(),
            ONCHAIN_WALLETS.test,
            GENEROUS_FEE,
            CALLER_PAYS,
        )
        .await
        .expect("deploy counter")
        .address;
    println!("counter deployed at: {counter}");
    settle().await;

    // Fresh deploy: slot 0 (count) reads as zero.
    let slot0 = chain
        .get_storage_at(&counter, alloy_primitives::U256::ZERO)
        .await
        .expect("get_storage_at slot 0 (pre-increment)");
    println!("slot 0 before increment: {slot0}");
    assert_eq!(slot0, alloy_primitives::U256::ZERO);

    // `call` polls for the receipt, so the incremented state is committed on return.
    chain
        .call(
            &counter,
            selector("increment()"),
            ONCHAIN_WALLETS.test,
            GENEROUS_FEE,
        )
        .await
        .expect("increment");
    settle().await;

    let slot0 = chain
        .get_storage_at(&counter, alloy_primitives::U256::ZERO)
        .await
        .expect("get_storage_at slot 0 (post-increment)");
    println!("slot 0 after increment: {slot0}");
    assert_eq!(
        slot0,
        alloy_primitives::U256::from(1u64),
        "expected count (slot 0) == 1 after a single increment"
    );
}

#[tokio::test]
#[ignore = "live: requires Nile RPC + funded MNEMONIC_TEST index 0 (coin 195)"]
async fn live_transfer_funds_on_nile() {
    // 1 TRX in sun. The sender also pays the bandwidth fee, and the 1-TRX account-activation fee
    // the first time the recipient address is seen on chain.
    const AMOUNT_SUN: u64 = 1_000_000;

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
    let to = chain
        .wallet_address(ONCHAIN_WALLETS.recipient)
        .await
        .expect("derive recipient wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    println!("balance:     {balance} sun");
    println!("recipient:   {to}");
    assert!(
        balance > AMOUNT_SUN,
        "fund {who} on Nile first (holds {balance} sun, needs more than {AMOUNT_SUN})"
    );

    let before = chain.balance(&to).await.expect("read recipient balance");
    let txid = chain
        .transfer_funds(&to, "TRX", AMOUNT_SUN, ONCHAIN_WALLETS.test)
        .await
        .expect("native transfer");
    println!("transfer txID: {txid}");
    assert_eq!(txid.len(), 64, "expected a 32-byte txID in hex");
    settle().await;

    let after = chain.balance(&to).await.expect("read recipient balance");
    println!("recipient balance: {before} -> {after} sun");
    assert_eq!(
        after,
        before + AMOUNT_SUN,
        "expected the recipient's balance to rise by exactly the transferred amount"
    );
}

#[tokio::test]
#[ignore = "live: requires Nile RPC + funded MNEMONIC_TEST index 0 (coin 195)"]
async fn live_get_code_on_nile() {
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

    // A funded wallet is an ordinary account, so it carries no runtime bytecode.
    let eoa_code = chain.get_code(&who).await.expect("get_code eoa");
    println!("eoa code len: {}", eoa_code.len());
    assert!(eoa_code.is_empty(), "an ordinary account has no code");

    // A freshly deployed contract does: get_code returns its runtime.
    let counter = chain
        .deploy_create(
            counter_bytecode(),
            Bytes::new(),
            ONCHAIN_WALLETS.test,
            GENEROUS_FEE,
            CALLER_PAYS,
        )
        .await
        .expect("deploy counter")
        .address;
    println!("counter deployed at: {counter}");
    settle().await;

    let code = chain.get_code(&counter).await.expect("get_code contract");
    println!("counter code len: {}", code.len());
    assert!(
        !code.is_empty(),
        "a deployed contract reports non-empty runtime bytecode"
    );
}

#[tokio::test]
#[ignore = "live: requires Nile RPC network access"]
async fn live_raw_request_eth_chain_id_on_nile() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let chain: TronChain = NILE.rpc(wallets).into();

    // The Ethereum-compatible JSON-RPC escape hatch: `eth_chainId` returns a `0x`-prefixed hex id.
    let id = chain
        .raw_request("eth_chainId", serde_json::json!([]))
        .await
        .expect("eth_chainId over the JSON-RPC escape hatch");
    println!("eth_chainId: {id}");
    let s = id.as_str().expect("eth_chainId returns a hex string");
    assert!(s.starts_with("0x"), "expected a 0x-prefixed chain id");
    assert!(
        u64::from_str_radix(s.trim_start_matches("0x"), 16).is_ok(),
        "chain id parses as hex"
    );
}

#[tokio::test]
#[ignore = "live: requires Nile RPC network access"]
async fn live_wallet_request_getnowblock_on_nile() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let chain: TronChain = NILE.rpc(wallets).into();

    // The native java-tron REST escape hatch: `/wallet/getnowblock` returns the head block.
    let block = chain
        .wallet_request("getnowblock", serde_json::json!({}))
        .await
        .expect("getnowblock over the REST escape hatch");
    let number = block["block_header"]["raw_data"]["number"]
        .as_u64()
        .expect("head block carries a number");
    println!("getnowblock number: {number}");
    assert!(number > 0, "expected a positive block number");
}

#[tokio::test]
#[ignore = "live: requires Nile RPC + funded MNEMONIC_TEST index 0 (coin 195)"]
async fn live_sign_and_broadcast_raw_transfer_on_nile() {
    // 1 TRX in sun.
    const AMOUNT_SUN: u64 = 1_000_000;

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
    // java-tron rejects a transfer to the sender's own address, so this raw transfer targets the
    // recipient wallet, exactly as the typed `transfer_funds` test does.
    let to = chain
        .wallet_address(ONCHAIN_WALLETS.recipient)
        .await
        .expect("derive recipient wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    println!("balance:     {balance} sun");
    println!("recipient:   {to}");
    assert!(
        balance > AMOUNT_SUN,
        "fund {who} on Nile first (holds {balance} sun, needs more than {AMOUNT_SUN})"
    );

    // Build the unsigned native transfer at the node via the REST escape hatch, then drive the raw
    // sign + broadcast pair by hand (what `transfer_funds` does internally).
    let unsigned = chain
        .wallet_request(
            "createtransaction",
            serde_json::json!({
                "owner_address": who.to_hex(),
                "to_address": to.to_hex(),
                "amount": AMOUNT_SUN,
                "visible": false,
            }),
        )
        .await
        .expect("createtransaction");
    assert!(
        unsigned["txID"].as_str().is_some(),
        "the node returns an unsigned transaction carrying a txID"
    );

    let signed = chain
        .sign_transaction(unsigned, ONCHAIN_WALLETS.test)
        .await
        .expect("sign the unsigned transfer");
    assert!(
        signed["signature"][0].as_str().is_some(),
        "signing populates the signature array"
    );

    let before = chain.balance(&to).await.expect("read recipient balance");
    let txid = chain
        .broadcast_transaction(signed)
        .await
        .expect("broadcast the signed transfer");
    println!("raw transfer txID: {txid}");
    assert_eq!(txid.len(), 64, "expected a 32-byte txID in hex");
    settle().await;

    let after = chain.balance(&to).await.expect("read recipient balance");
    println!("recipient balance: {before} -> {after} sun");
    assert_eq!(
        after,
        before + AMOUNT_SUN,
        "expected the recipient's balance to rise by exactly the transferred amount"
    );
}

/// The energy a live Tron backend metered. A [`TronCompute::Gas`] here would mean the RPC arm is
/// reporting revm's unit, which no live node produces.
fn energy(compute: TronCompute) -> u64 {
    let TronCompute::Energy(e) = compute else {
        panic!("a live node meters energy, not gas: got {compute:?}");
    };
    assert!(e > 0, "a contract transaction burns energy");
    e
}

/// Assert a forecast and the figure the executed operation reported are the same order of
/// magnitude. Exact equality is not on offer: the node estimates against the state at the head it
/// saw, and its penalty energy depends on how recently the contract was touched.
fn assert_close(forecast: u64, actual: u64, what: &str) {
    assert!(
        forecast * 2 >= actual && forecast <= actual * 2,
        "{what}: forecast {forecast} energy, receipt says {actual}: not within 2x"
    );
}

/// Nile blocks are ~3s; give a broadcast tx time to confirm before reading state.
async fn settle() {
    tokio::time::sleep(Duration::from_secs(8)).await;
}
