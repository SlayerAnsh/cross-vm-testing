//! Quickstart: load wallet mnemonics, derive a per-VM signer, and broadcast under a per-wallet
//! lock through the testing environment.
//!
//! Run with: `cargo run -p cross-vm-framework --example wallet_quickstart`
//!
//! The shared `TEST_WALLETS` roster is all `auto`, so wallets get fresh generated mnemonics and
//! the example is self-contained. On a real network use `env_mnemonic("VAR")` rows and load the
//! secrets from a `.env` file (e.g. `dotenvy::dotenv()`) before building the factory.

use std::rc::Rc;

use cross_vm_framework::prelude::*;
use solana_system_interface::instruction::transfer;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let wallets = Rc::new(WalletFactory::from_roster(TestWallets::SPECS).unwrap());

    let mut env = MultiChainEnv::new("wallet-demo", wallets.clone());
    env.inject("sol", AnyChain::from(SOLANA_DEVNET.mock(wallets)));
    let mut env = env.start().await.expect("start");

    let alice = env
        .solana("sol")
        .unwrap()
        .wallet_address(TEST_WALLETS.alice)
        .await
        .unwrap();
    let bob = env
        .solana("sol")
        .unwrap()
        .wallet_address(TEST_WALLETS.bob)
        .await
        .unwrap();
    println!("alice: {alice}");
    println!("bob:   {bob}");

    env.solana("sol")
        .unwrap()
        .set_balance(&alice, "SOL", 10_000_000_000)
        .await
        .unwrap();
    let ix = transfer(&alice, &bob, 1_000_000_000); // 1 SOL
    env.solana("sol")
        .unwrap()
        .send_transaction(vec![ix], TEST_WALLETS.alice)
        .await
        .expect("transfer");

    println!(
        "alice balance: {}",
        env.solana("sol").unwrap().balance(&alice).await.unwrap()
    );
    println!(
        "bob balance:   {}",
        env.solana("sol").unwrap().balance(&bob).await.unwrap()
    );
}
