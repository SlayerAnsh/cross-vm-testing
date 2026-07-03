//! Shared wallet factory and per-VM funding helpers for the example test crates.

use std::rc::Rc;

use cross_vm_framework::prelude::*;

/// A shared wallet factory. The test roster is all `auto`, so every wallet gets a fresh
/// generated mnemonic (stable within a run, since this single factory is shared by all chains).
pub fn test_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

/// An empty wallet factory for tests that only use `new_account`.
pub fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
}

/// Fund the `alice` wallet so it can pay for deploys/txs on the gas-charging VMs.
pub async fn fund_alice(chain: &mut AnyChain) {
    fund_user(chain, TEST_WALLETS.alice).await;
}

/// Fund an arbitrary wallet label on an EVM chain with gas money (100 ETH).
pub async fn fund_evm(chain: &mut AnyChain, label: WalletLabel<'_>) {
    if let AnyChain::Evm(c) = chain {
        let a = c.wallet_address(label).await.unwrap();
        c.set_balance(
            &a,
            cross_vm_solidity::U256::from(10u64).pow(cross_vm_solidity::U256::from(20)),
        )
        .await
        .unwrap();
    }
}

/// Fund a wallet label on any VM so it can pay for deploys/txs.
pub async fn fund_user(chain: &mut AnyChain, label: WalletLabel<'_>) {
    match chain {
        AnyChain::Evm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(
                &a,
                cross_vm_solidity::U256::from(10u64).pow(cross_vm_solidity::U256::from(20)),
            )
            .await
            .unwrap();
        }
        AnyChain::Svm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(&a, 100_000_000_000).await.unwrap(); // 100 SOL
        }
        AnyChain::CosmWasm(c) => {
            let a = c.wallet_address(label).await.unwrap();
            let _ = c.set_balance(&a, 1_000_000_000_000).await;
        }
        AnyChain::Tron(c) => {
            let a = c.wallet_address(label).await.unwrap();
            c.set_balance(&a, 100_000_000_000_000).await.unwrap(); // 100M TRX in sun
        }
    }
}
