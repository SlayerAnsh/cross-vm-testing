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
use cross_vm_cosmwasm::CwChain;
use cross_vm_macros::define_wallet_roster;

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
    };
}

const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");

const COUNTER_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/cosmwasm/counter/artifacts/counter.wasm"
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

    let code_id = chain
        .store_code_wasm(COUNTER_WASM.to_vec(), ONCHAIN_WALLETS.test)
        .await
        .expect("store_code_wasm");
    println!("stored code id:     {code_id}");

    let addr = chain
        .instantiate(
            code_id,
            InstantiateMsg {},
            ONCHAIN_WALLETS.test,
            &[],
            "counter",
        )
        .await
        .expect("instantiate");
    println!("instantiated at:    {addr}");
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count");
    assert_eq!(resp.count, 0, "fresh counter starts at 0");

    chain
        .execute_contract(&addr, ExecuteMsg::Increment {}, ONCHAIN_WALLETS.test, &[])
        .await
        .expect("increment");
    let resp: CountResponse = chain
        .query_wasm_smart(&addr, QueryMsg::GetCount {})
        .await
        .expect("query count after increment");
    println!("count after increment: {}", resp.count);
    assert_eq!(resp.count, 1, "increment should raise the count to 1");
}
