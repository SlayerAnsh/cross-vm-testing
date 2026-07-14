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
use cross_vm_cosmwasm::{CwChain, CwError, CwGas, CwGasLimit};
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

/// A live node can simulate any op it can broadcast, so each RPC estimate must be present
/// (unlike the mock's `None`) and within a sane band of the gas the op then actually meters.
/// Simulation runs against the latest committed state, so the two figures are never far apart;
/// a divergence beyond 2x means the estimator is wired to the wrong quantity or the wrong op.
fn assert_estimated(op: &str, estimated: Option<u64>, metered: u64) {
    let est =
        estimated.unwrap_or_else(|| panic!("rpc {op} estimate must report a figure, got None"));
    assert!(est > 0, "rpc {op} estimated zero gas");
    assert!(
        (metered / 2..=metered.saturating_mul(2)).contains(&est),
        "rpc {op} estimate {est} implausibly far from metered {metered}"
    );
    println!("{op} estimate: {est} gas (metered {metered})");
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

    let est_store = chain
        .estimate_store_code(COUNTER_WASM.to_vec(), ONCHAIN_WALLETS.test)
        .await
        .expect("estimate store_code");
    let stored = chain
        .store_code(
            COUNTER_WASM.to_vec(),
            ONCHAIN_WALLETS.test,
            CwGasLimit::Estimated,
        )
        .await
        .expect("store_code");
    assert!(!stored.tx_hash.is_empty(), "tx hash should be non-empty");
    assert_metered("store_code", stored.gas);
    assert_estimated("store_code", est_store, stored.gas.expect("metered").used);
    println!(
        "stored code id:     {} (tx {})",
        stored.code_id, stored.tx_hash
    );

    let est_init = chain
        .estimate_instantiate(
            stored.code_id,
            InstantiateMsg {},
            ONCHAIN_WALLETS.test,
            &[],
            "counter",
        )
        .await
        .expect("estimate instantiate");
    let instantiated = chain
        .instantiate(
            stored.code_id,
            InstantiateMsg {},
            ONCHAIN_WALLETS.test,
            &[],
            "counter",
            CwGasLimit::Estimated,
        )
        .await
        .expect("instantiate");
    assert!(
        !instantiated.tx_hash.is_empty(),
        "tx hash should be non-empty"
    );
    assert_metered("instantiate", instantiated.gas);
    assert_estimated(
        "instantiate",
        est_init,
        instantiated.gas.expect("metered").used,
    );
    let addr = instantiated.address;
    println!("instantiated at:    {addr} (tx {})", instantiated.tx_hash);
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count");
    assert_eq!(resp.count, 0, "fresh counter starts at 0");

    let est_exec = chain
        .estimate_execute_contract(&addr, ExecuteMsg::Increment {}, ONCHAIN_WALLETS.test, &[])
        .await
        .expect("estimate increment");
    let exec = chain
        .execute_contract(
            &addr,
            ExecuteMsg::Increment {},
            ONCHAIN_WALLETS.test,
            &[],
            CwGasLimit::Estimated,
        )
        .await
        .expect("increment");
    assert_metered("execute_contract", exec.gas);
    assert_estimated(
        "execute_contract",
        est_exec,
        exec.gas.expect("metered").used,
    );
    let tx_hash = exec.tx_hash;
    assert!(!tx_hash.is_empty(), "tx hash should be non-empty");
    println!("increment tx hash: {tx_hash}");
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count after increment");
    println!("count after increment: {}", resp.count);
    assert_eq!(resp.count, 1, "increment should raise the count to 1");

    // `Exact` is honored verbatim: a limit the execution cannot fit in fails the transaction
    // (the node's gas meter trips) instead of being quietly topped up. This is the half of the
    // contract the mock cannot show, because it has no meter to trip.
    let err = chain
        .execute_contract(
            &addr,
            ExecuteMsg::Increment {},
            ONCHAIN_WALLETS.test,
            &[],
            CwGasLimit::Exact(1),
        )
        .await
        .expect_err("a 1-gas limit cannot cover an increment");
    assert!(
        matches!(err, CwError::Execute(_)),
        "out of gas is an execute failure, got {err:?}"
    );
    println!("Exact(1) rejected as expected: {err}");
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count after the rejected increment");
    assert_eq!(
        resp.count, 1,
        "the out-of-gas increment must not have landed"
    );
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

    // `transfer_funds` returns only a hash (no gas figure to compare against), so bound the
    // estimate by what is known: nonzero, and comfortably under the `Exact` limit the send below
    // declares, or that send could not succeed.
    const TRANSFER_LIMIT: u64 = 200_000;
    let est = chain
        .estimate_transfer_funds(&to, denom, AMOUNT, ONCHAIN_WALLETS.test)
        .await
        .expect("estimate transfer")
        .expect("rpc transfer estimate must report a figure, got None");
    assert!(est > 0, "rpc transfer estimated zero gas");
    assert!(
        est <= TRANSFER_LIMIT,
        "rpc transfer estimate {est} exceeds the {TRANSFER_LIMIT} gas limit this send declares"
    );
    println!("transfer estimate: {est} gas");

    let before = chain.balance(&to).await.expect("read recipient balance");
    let tx_hash = chain
        .transfer_funds(
            &to,
            denom,
            AMOUNT,
            ONCHAIN_WALLETS.test,
            CwGasLimit::Exact(TRANSFER_LIMIT),
        )
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
