//! Read-only live RPC against Osmosis testnet (`osmo-test-5`).
//!
//! Run (needs network):
//!
//! ```text
//! cargo run -p cross-vm-cosmwasm --example cosmwasm_rpc_quickstart
//! ```

use std::rc::Rc;

use cross_vm_core::{ChainProvider, WalletFactory};
use cross_vm_cosmwasm::chains::OSMOSIS_TESTNET;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(&[]).expect("empty roster"));
    let mut chain = OSMOSIS_TESTNET.rpc(wallets);

    let height = chain.block_height().await;
    println!(
        "{} ({}) latest block height: {height}",
        OSMOSIS_TESTNET.name, OSMOSIS_TESTNET.chain_id
    );

    let who = chain.new_account("alice").await;
    match chain.balance(&who).await {
        Ok(amount) => println!(
            "balance of {who}: {amount} {}",
            OSMOSIS_TESTNET.native_denom
        ),
        Err(e) => println!("balance query failed: {e}"),
    }
}
