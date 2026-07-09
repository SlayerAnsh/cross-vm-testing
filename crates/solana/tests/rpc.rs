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

use cross_vm_core::WalletFactory;
use cross_vm_solana::chains::SOLANA_DEVNET;
use solana_address::Address;

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
