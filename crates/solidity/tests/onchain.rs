//! Live on-chain test: deploy + increment + read a Solidity `Counter` on Base Sepolia, signed
//! by the `test` wallet derived from `MNEMONIC_TEST` in the workspace `.env`.
//!
//! Ignored by default (needs network access and a funded key). Run with:
//!   cargo test -p cross-vm-solidity --test onchain -- --ignored --nocapture
//!
//! The `test` wallet is index 0 of `MNEMONIC_TEST` (`m/44'/60'/0'/0/0`); fund that address on
//! Base Sepolia first (the test prints it and fails fast if the balance is zero).

use std::rc::Rc;

use alloy::sol_types::SolCall;
use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_macros::define_wallet_roster;
use cross_vm_solidity::chains::BASE_SEPOLIA;
use cross_vm_solidity::{Bytes, EvmChain, U256};

define_wallet_roster! {
    pub const ONCHAIN_WALLETS: OnchainWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
    };
}

alloy::sol!(
    #[sol(abi)]
    Counter,
    "../../contracts/solidity/out/Counter.sol/Counter.json"
);

const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");

async fn read_count(chain: &EvmChain, contract: &cross_vm_solidity::Address) -> u64 {
    let out = chain
        .static_call(contract, Bytes::from(Counter::countCall {}.abi_encode()))
        .await
        .expect("static_call count");
    Counter::countCall::abi_decode_returns(&out)
        .expect("decode count")
        .saturating_to::<u64>()
}

#[tokio::test]
#[ignore = "live: requires Base Sepolia RPC + funded MNEMONIC_TEST index 0"]
async fn live_counter_on_base_sepolia() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: EvmChain = BASE_SEPOLIA.rpc(wallets).into();

    let who = chain
        .wallet_address(ONCHAIN_WALLETS.test)
        .await
        .expect("derive test wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    println!("balance:     {balance} wei");
    assert!(
        balance > U256::ZERO,
        "wallet {who} has no Base Sepolia ETH; fund it (index 0 of MNEMONIC_TEST) and retry"
    );

    let contract = chain
        .deploy_create(
            Counter::BYTECODE.clone(),
            Bytes::new(),
            ONCHAIN_WALLETS.test,
        )
        .await
        .expect("deploy_create");
    println!("deployed Counter at: {contract}");
    assert_eq!(
        read_count(&chain, &contract).await,
        0,
        "fresh counter starts at 0"
    );

    let exec = chain
        .call(
            &contract,
            Bytes::from(Counter::incrementCall {}.abi_encode()),
            ONCHAIN_WALLETS.test,
        )
        .await
        .expect("increment");
    let tx_hash = exec
        .tx_hash
        .expect("live RPC call returns a broadcast tx hash");
    println!("increment tx hash: {tx_hash}");
    let count = read_count(&chain, &contract).await;
    println!("count after increment: {count}");
    assert_eq!(count, 1, "increment should raise the count to 1");
}

#[tokio::test]
#[ignore = "live: requires Base Sepolia RPC + funded MNEMONIC_TEST index 0"]
async fn live_transfer_funds_on_base_sepolia() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(OnchainWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: EvmChain = BASE_SEPOLIA.rpc(wallets).into();

    let who = chain
        .wallet_address(ONCHAIN_WALLETS.test)
        .await
        .expect("derive test wallet");
    let balance = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    println!("balance:     {balance} wei");
    assert!(
        balance > U256::ZERO,
        "wallet {who} has no Base Sepolia ETH; fund it (index 0 of MNEMONIC_TEST) and retry"
    );

    // Send a dust amount back to the sender: no second funded key is needed, and the only balance
    // change is the gas the transfer itself burns.
    let amount = U256::from(1_000u64);
    let start = chain.balance(&who).await.expect("read balance");
    let tx_hash = chain
        .transfer_funds(&who, "ETH", amount, ONCHAIN_WALLETS.test)
        .await
        .expect("transfer_funds");
    println!("transfer tx hash: {tx_hash}");
    assert!(
        tx_hash.starts_with("0x"),
        "hash `{tx_hash}` is not 0x-prefixed"
    );
    assert_eq!(tx_hash.len(), 66, "hash `{tx_hash}` is not 32 bytes of hex");

    let end = chain.balance(&who).await.expect("read balance");
    println!("balance after self-transfer: {end} wei (was {start})");
    assert!(end < start, "the transfer should at least have burnt gas");
}
