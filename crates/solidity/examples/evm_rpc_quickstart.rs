//! Read-only live RPC against Ethereum Sepolia.
//!
//! Run (needs network):
//!
//! ```text
//! cargo run -p cross-vm-solidity --example evm_rpc_quickstart
//! ```

use std::rc::Rc;

use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_solidity::chains::SEPOLIA;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let mut chain = SEPOLIA.rpc(wallets);

    let height = chain.block_height().await;
    println!(
        "{} (chain id {}) latest block number: {height}",
        SEPOLIA.name, SEPOLIA.chain_id
    );

    let who = chain.new_account("alice").await;
    match chain.balance(&who).await {
        Ok(amount) => println!("balance of {who}: {amount} wei {}", SEPOLIA.native_symbol),
        Err(e) => println!("balance query failed: {e}"),
    }
}
