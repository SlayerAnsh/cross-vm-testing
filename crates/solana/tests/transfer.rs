//! Integration test: airdrop -> transfer -> balance through the Solana provider,
//! using a System Program transfer instruction (no custom program needed).

use cross_vm_core::ChainProvider;
use cross_vm_solana::chains::SOLANA_LOCALNET;
use solana_system_interface::instruction::transfer;

#[test]
fn airdrop_transfer_balance() {
    let mut chain = SOLANA_LOCALNET.mock();

    // new_account airdrops the default funding to each.
    let alice = chain.new_account("alice");
    let bob = chain.new_account("bob");

    let alice_start = chain.balance(&alice).unwrap();
    let bob_start = chain.balance(&bob).unwrap();
    assert!(alice_start > 0);

    let amount = 1_000_000_000; // 1 SOL
    let ix = transfer(&alice, &bob, amount);
    chain
        .execute(&solana_system_interface::program::ID, vec![ix], &alice)
        .expect("transfer");

    // Bob gained exactly `amount`; Alice lost `amount` plus any fee.
    assert_eq!(chain.balance(&bob).unwrap(), bob_start + amount);
    assert!(chain.balance(&alice).unwrap() <= alice_start - amount);
}
