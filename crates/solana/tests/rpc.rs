//! Live read-only RPC tests against Solana Devnet.
//!
//! These hit a real cluster, so they are `#[ignore]` by default and skipped by the offline
//! suite. Run them explicitly with network access:
//!
//! ```text
//! cargo test -p cross-vm-solana --test rpc -- --ignored
//! ```

use std::rc::Rc;
use std::str::FromStr;

use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_macros::define_wallet_roster;
use cross_vm_solana::chains::SOLANA_DEVNET;
use cross_vm_solana::SvmChain;
use solana_address::Address;
use solana_signature::Signature;
use solana_system_interface::instruction::transfer;

define_wallet_roster! {
    pub const RPC_WALLETS: RpcWallets = {
        test: env_mnemonic("MNEMONIC_TEST") @ 0,
    };
}

/// Workspace `.env`, holding the funded `MNEMONIC_TEST` the live signing test derives from.
const ENV_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

#[tokio::test]
#[ignore = "requires network access to Solana Devnet"]
async fn live_slot_is_nonzero() {
    let chain = SOLANA_DEVNET.rpc(empty_wallets());
    let slot = chain.try_block_height().await.expect("query Devnet slot");
    assert!(slot > 0, "expected a nonzero slot, got {slot}");
}

#[tokio::test]
#[ignore = "requires network access to Solana Devnet"]
async fn live_raw_request_get_slot() {
    let chain: SvmChain = SOLANA_DEVNET.rpc(empty_wallets()).into();
    // The raw JSON-RPC escape hatch reaches any method, here the same `getSlot` `try_block_height`
    // wraps, straight through as untyped JSON.
    let result = chain
        .raw_request("getSlot", serde_json::json!([]))
        .await
        .expect("raw getSlot");
    let slot = result.as_u64().expect("getSlot returns a number");
    assert!(slot > 0, "expected a nonzero slot, got {slot}");
}

#[tokio::test]
#[ignore = "live: requires Solana Devnet RPC + funded MNEMONIC_TEST index 0"]
async fn live_sign_and_send_raw_transaction() {
    dotenvy::from_path(ENV_PATH).unwrap_or_else(|e| panic!("load {ENV_PATH}: {e}"));
    let wallets = Rc::new(
        WalletFactory::from_roster(RpcWallets::SPECS)
            .unwrap_or_else(|e| panic!("resolve roster: {e}")),
    );
    let chain: SvmChain = SOLANA_DEVNET.rpc(wallets).into();

    let who = chain
        .wallet_address(RPC_WALLETS.test)
        .await
        .expect("derive test wallet");
    let start = chain.balance(&who).await.expect("read balance");
    println!("test wallet: {who}");
    assert!(
        start > 0,
        "wallet {who} has no Devnet SOL; airdrop it (index 0 of MNEMONIC_TEST) and retry"
    );

    // Sign a dust self-transfer through the raw escape hatch (a live blockhash is fetched under it),
    // then broadcast the signed bytes: the two halves of a custom-transaction round trip.
    let raw = chain
        .sign_transaction(vec![transfer(&who, &who, 1)], RPC_WALLETS.test)
        .await
        .expect("sign_transaction");
    println!("signed raw tx: {} bytes", raw.len());
    assert!(!raw.is_empty(), "a signed transaction cannot be empty");

    let signature = chain
        .send_raw_transaction(&raw)
        .await
        .expect("send_raw_transaction");
    println!("confirmed signature: {signature}");
    Signature::from_str(&signature).expect("a confirmed base58 signature");
}

#[tokio::test]
#[ignore = "requires network access to Solana Devnet"]
async fn live_get_account_decodes_system_program() {
    let chain = SOLANA_DEVNET.rpc(empty_wallets());
    let system = Address::from_str("11111111111111111111111111111111").unwrap();
    let account = chain
        .get_account(&system)
        .await
        .expect("getAccountInfo")
        .expect("system program account exists");
    assert!(account.executable, "system program should be executable");
}

#[tokio::test]
#[ignore = "requires network access to Solana Devnet"]
async fn live_get_account_data_matches_account_bytes() {
    let chain = SOLANA_DEVNET.rpc(empty_wallets());
    let system = Address::from_str("11111111111111111111111111111111").unwrap();
    let account = chain
        .get_account(&system)
        .await
        .expect("getAccountInfo")
        .expect("system program account exists");
    let data = chain
        .get_account_data(&system)
        .await
        .expect("getAccountInfo")
        .expect("system program account exists");
    assert_eq!(data, account.data);
}

#[tokio::test]
#[ignore = "requires network access to Solana Devnet"]
async fn live_get_account_data_slice_matches_prefix() {
    let chain = SOLANA_DEVNET.rpc(empty_wallets());
    let system = Address::from_str("11111111111111111111111111111111").unwrap();
    let data = chain
        .get_account_data(&system)
        .await
        .expect("getAccountInfo")
        .expect("system program account exists");
    let n = data.len().min(8);

    let slice = chain
        .get_account_data_slice(&system, 0, n)
        .await
        .expect("getAccountInfo")
        .expect("slice within data");
    assert_eq!(slice, data[..n]);

    // A window past the end of the data is reported as absent, not truncated.
    assert!(chain
        .get_account_data_slice(&system, data.len() + 1, 1)
        .await
        .expect("getAccountInfo")
        .is_none());
}
