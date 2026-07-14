//! Live on-chain test: store + instantiate + increment + read a CosmWasm `Counter` on Osmosis
//! testnet, signed by the `test` wallet derived from `MNEMONIC_TEST` in the workspace `.env`.
//!
//! Ignored by default (needs network access and a funded key). Build the wasm first, then run:
//!   make compile-cosmwasm
//!   cargo test -p cross-vm-cosmwasm --test onchain -- --ignored --nocapture
//!
//! NOTE: the `test` wallet is the Cosmos address (BIP-44 coin 118, `osmo1...`), which is
//! DIFFERENT from the EVM address (coin 60) derived from the same mnemonic. Fund that osmo
//! address with testnet OSMO first (the test prints it and fails fast if the balance is zero).

use std::rc::Rc;

use counter::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS_TESTNET;
use cross_vm_cosmwasm::{CwChain, CwGas};
use cross_vm_macros::define_wallet_roster;

/// A live node meters every transaction, so each RPC op must come back with a real gas figure:
/// present (unlike the mock, which cannot meter and reports `None`) and nonzero (no Cosmos tx
/// executes on zero gas, and no chain with a nonzero `gas_price` charges a zero fee).
fn assert_metered(op: &str, gas: Option<CwGas>) {
    let gas = gas.unwrap_or_else(|| panic!("rpc {op} must report a gas figure, got None"));
    assert!(gas.used > 0, "rpc {op} reported zero gas used");
    assert!(gas.fee > 0, "rpc {op} reported a zero fee");
    println!("{op} cost: {} gas, fee {}", gas.used, gas.fee);
}

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
        // Transfer recipient: account index 1 off the same mnemonic, so it needs no funding
        // of its own (a bank send creates the account on first receipt).
        sink: env_mnemonic("MNEMONIC_TEST") @ 1,
    };
}

const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");

const COUNTER_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/cosmwasm/artifacts/counter.wasm"
));

#[tokio::test]
#[ignore = "live: requires Osmosis testnet RPC + funded MNEMONIC_TEST osmo address (coin 118)"]
async fn live_counter_on_osmosis_testnet() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: CwChain = OSMOSIS_TESTNET.rpc(wallets).into();

    let who = chain
        .wallet_address(ONCHAIN_WALLETS.test)
        .await
        .expect("derive test wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet (osmo): {who}");
    println!("balance:            {balance} uosmo");
    assert!(
        balance > 0,
        "wallet {who} has no testnet OSMO; fund this osmo address (BIP-44 coin 118, distinct \
         from the EVM address) and retry"
    );

    let stored = chain
        .store_code(COUNTER_WASM.to_vec(), ONCHAIN_WALLETS.test)
        .await
        .expect("store_code");
    assert!(!stored.tx_hash.is_empty(), "tx hash should be non-empty");
    assert_metered("store_code", stored.gas);
    println!(
        "stored code id:     {} (tx {})",
        stored.code_id, stored.tx_hash
    );

    let instantiated = chain
        .instantiate(
            stored.code_id,
            InstantiateMsg {},
            ONCHAIN_WALLETS.test,
            &[],
            "counter",
        )
        .await
        .expect("instantiate");
    assert!(
        !instantiated.tx_hash.is_empty(),
        "tx hash should be non-empty"
    );
    assert_metered("instantiate", instantiated.gas);
    let addr = instantiated.address;
    println!("instantiated at:    {addr} (tx {})", instantiated.tx_hash);
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count");
    assert_eq!(resp.count, 0, "fresh counter starts at 0");

    let exec = chain
        .execute_contract(&addr, ExecuteMsg::Increment {}, ONCHAIN_WALLETS.test, &[])
        .await
        .expect("increment");
    assert_metered("execute_contract", exec.gas);
    let tx_hash = exec.tx_hash;
    assert!(!tx_hash.is_empty(), "tx hash should be non-empty");
    println!("increment tx hash: {tx_hash}");
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count after increment");
    println!("count after increment: {}", resp.count);
    assert_eq!(resp.count, 1, "increment should raise the count to 1");
}

#[tokio::test]
#[ignore = "live: requires Osmosis testnet RPC + funded MNEMONIC_TEST osmo address (coin 118)"]
async fn live_transfer_funds_on_osmosis_testnet() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: CwChain = OSMOSIS_TESTNET.rpc(wallets).into();
    let denom = chain.chain_info().native_denom;

    // A bank send costs the fee plus the amount; keep the amount tiny so a faucet grant covers
    // many runs.
    const AMOUNT: u128 = 1_000;

    let from = chain
        .wallet_address(ONCHAIN_WALLETS.test)
        .await
        .expect("derive test wallet");
    let to = chain
        .wallet_address(ONCHAIN_WALLETS.sink)
        .await
        .expect("derive sink wallet");
    let balance = chain.balance(&from).await.expect("read balance");
    println!("sender (osmo):   {from}");
    println!("recipient (osmo): {to}");
    println!("balance:          {balance} {denom}");
    assert!(
        balance > AMOUNT,
        "wallet {from} has no testnet OSMO; fund this osmo address (BIP-44 coin 118, distinct \
         from the EVM address) and retry"
    );

    let before = chain.balance(&to).await.expect("read recipient balance");
    let tx_hash = chain
        .transfer_funds(&to, denom, AMOUNT, ONCHAIN_WALLETS.test)
        .await
        .expect("transfer_funds");
    assert!(!tx_hash.is_empty(), "tx hash should be non-empty");
    println!("transfer tx hash: {tx_hash}");

    let after = chain.balance(&to).await.expect("read recipient balance");
    assert_eq!(
        after,
        before + AMOUNT,
        "recipient should be credited the transferred amount"
    );
}
