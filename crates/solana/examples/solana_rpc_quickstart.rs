//! Read-only live RPC against Solana Devnet.
//!
//! Run (needs network):
//!
//! ```text
//! cargo run -p cross-vm-solana --example solana_rpc_quickstart
//! ```

use std::rc::Rc;

use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_solana::chains::SOLANA_DEVNET;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let mut chain = SOLANA_DEVNET.rpc(wallets);

    let slot = chain.block_height().await;
    println!(
        "{} (chain id {}) current slot: {slot}",
        SOLANA_DEVNET.name, SOLANA_DEVNET.chain_id
    );

    let who = chain.new_account("alice").await;
    match chain.balance(&who).await {
        Ok(lamports) => println!("balance of {who}: {lamports} lamports"),
        Err(e) => println!("balance query failed: {e}"),
    }
}
