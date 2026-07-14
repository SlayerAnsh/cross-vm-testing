//! Tests for the cross-VM testing environment.

use std::rc::Rc;

use crate::prelude::*;
#[cfg(feature = "evm")]
use cross_vm_solidity::U256;

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
}

/// The framework roster (`alice`, `bob`, ...): `transfer_funds` signs with a wallet label, so
/// its tests need a roster with resolvable keys rather than the empty one.
fn test_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("test roster"))
}

#[test]
fn inject_and_access() {
    let wallets = empty_wallets();
    let mut env = MultiChainEnv::new("t", wallets.clone());
    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets.clone())));
    env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets)));
    assert_eq!(env.len(), 2);
    assert!(env.cosmwasm("osmosis").is_ok());
    assert!(env.evm("eth").is_ok());
}

#[test]
fn unknown_label_errors() {
    let env = MultiChainEnv::new("t", empty_wallets());
    let mut env = env;
    assert!(matches!(
        env.cosmwasm("nope"),
        Err(EnvError::UnknownChain(_))
    ));
}

#[test]
fn wrong_vm_errors() {
    let wallets = empty_wallets();
    let mut env = MultiChainEnv::new("t", wallets.clone());
    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets)));
    assert!(matches!(env.evm("osmosis"), Err(EnvError::WrongVm { .. })));
}

#[test]
fn fund_wrong_vm_is_eager_error() {
    let wallets = empty_wallets();
    let mut env = MultiChainEnv::new("t", wallets.clone());
    env.inject("eth", AnyChain::from(ETHEREUM.mock(wallets)));
    let addr = cross_vm_cosmwasm::Addr::unchecked("osmo1xyz");
    let res = env.fund("eth", &addr, "uosmo", 1u128);
    assert!(matches!(res, Err(EnvError::WrongVm { .. })));
}

#[tokio::test]
async fn native_funding_mints_on_start() {
    let wallets = empty_wallets();
    let mut env = MultiChainEnv::new("t", wallets.clone());
    env.inject("osmosis", AnyChain::from(OSMOSIS.mock(wallets)));
    let alice = env.cosmwasm("osmosis").unwrap().new_account("alice").await;
    env.fund("osmosis", &alice, "uosmo", 9_000_000u128).unwrap();
    let mut env = env.start().await.expect("start");
    let bal = env
        .cosmwasm("osmosis")
        .unwrap()
        .balance(&alice)
        .await
        .unwrap();
    assert!(bal >= 9_000_000);
}

#[test]
fn chain_id_matches_typed_spec() {
    let wallets = empty_wallets();

    #[cfg(feature = "cw")]
    {
        let chain = CwChain::from(OSMOSIS.mock(wallets.clone()));
        let expected = chain.chain_info().chain_id().to_string();
        let any = AnyChain::from(chain);
        assert_eq!(any.chain_id(), expected);
    }
    #[cfg(feature = "evm")]
    {
        let chain = EvmChain::from(ETHEREUM.mock(wallets.clone()));
        let expected = chain.chain_info().chain_id().to_string();
        let any = AnyChain::from(chain);
        assert_eq!(any.chain_id(), expected);
    }
    #[cfg(feature = "solana")]
    {
        let chain = SvmChain::from(SOLANA_DEVNET.mock(wallets.clone()));
        let expected = chain.chain_info().chain_id().to_string();
        let any = AnyChain::from(chain);
        assert_eq!(any.chain_id(), expected);
    }
    #[cfg(feature = "tron")]
    {
        let chain = TronChain::from(TRON_NILE.mock(wallets.clone()));
        let expected = chain.chain_info().chain_id().to_string();
        let any = AnyChain::from(chain);
        assert_eq!(any.chain_id(), expected);
    }

    let _ = &wallets;
}

#[cfg(feature = "cw")]
#[tokio::test]
async fn transfer_funds_moves_native_funds_on_cosmwasm() {
    let mut chain = CwChain::from(OSMOSIS.mock(test_wallets()));
    let denom = OSMOSIS.native_denom;
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain.set_balance(&alice, denom, 1_000).await.unwrap();

    // The mock backends are `Rc`-backed, so the `AnyChain` clone drives the same chain the
    // typed handle reads balances from.
    let any = AnyChain::from(chain.clone());
    let hash = any
        .transfer_funds(&Account::from(bob.clone()), denom, 400, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert!(!hash.is_empty(), "transfer returned no tx hash");
    assert_eq!(chain.balance(&alice).await.unwrap(), 600);
    assert_eq!(chain.balance(&bob).await.unwrap(), 400);
}

#[cfg(feature = "evm")]
#[tokio::test]
async fn transfer_funds_moves_native_funds_on_evm() {
    let mut chain = EvmChain::from(ETHEREUM.mock(test_wallets()));
    let denom = ETHEREUM.native_symbol;
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, denom, U256::from(1_000u64))
        .await
        .unwrap();

    let any = AnyChain::from(chain.clone());
    let hash = any
        .transfer_funds(&Account::from(bob), denom, 400, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert!(!hash.is_empty(), "transfer returned no tx hash");
    assert_eq!(chain.balance(&alice).await.unwrap(), U256::from(600u64));
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(400u64));
}

#[cfg(feature = "solana")]
#[tokio::test]
async fn transfer_funds_moves_native_funds_on_solana() {
    let mut chain = SvmChain::from(SOLANA_DEVNET.mock(test_wallets()));
    let denom = SOLANA_DEVNET.native_symbol;
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, denom, 5_000_000_000)
        .await
        .unwrap();

    // This arm resolves `SvmComputeBudget::Estimated`, so the transfer also exercises the
    // simulate-then-cap path on the mock.
    let any = AnyChain::from(chain.clone());
    let hash = any
        .transfer_funds(
            &Account::from(bob),
            denom,
            1_000_000_000,
            TEST_WALLETS.alice,
        )
        .await
        .expect("transfer");

    assert!(!hash.is_empty(), "transfer returned no tx hash");
    assert_eq!(chain.balance(&bob).await.unwrap(), 1_000_000_000);
    // The sender also pays the signature fee, so its debit is at least the amount.
    assert!(chain.balance(&alice).await.unwrap() <= 4_000_000_000);
}

#[cfg(feature = "tron")]
#[tokio::test]
async fn transfer_funds_moves_native_funds_on_tron() {
    let mut chain = TronChain::from(TRON_NILE.mock(test_wallets()));
    let denom = TRON_NILE.native_symbol;
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain.set_balance(&alice, denom, 1_000).await.unwrap();

    // Tron's `transfer_funds` takes no limit (a `TransferContract` burns only bandwidth), so
    // this arm forwards nothing and the balances move exactly.
    let any = AnyChain::from(chain.clone());
    let hash = any
        .transfer_funds(&Account::from(bob), denom, 400, TEST_WALLETS.alice)
        .await
        .expect("transfer");

    assert!(!hash.is_empty(), "transfer returned no tx hash");
    assert_eq!(chain.balance(&alice).await.unwrap(), 600);
    assert_eq!(chain.balance(&bob).await.unwrap(), 400);
}

#[cfg(all(feature = "cw", feature = "evm"))]
#[tokio::test]
async fn transfer_funds_rejects_a_recipient_from_another_vm() {
    let chain = EvmChain::from(ETHEREUM.mock(test_wallets()));
    let recipient = Account::CosmWasm(cross_vm_cosmwasm::Addr::unchecked("osmo1xyz"));

    let err = AnyChain::from(chain)
        .transfer_funds(&recipient, ETHEREUM.native_symbol, 1, TEST_WALLETS.alice)
        .await
        .expect_err("a CosmWasm recipient on an EVM chain is the wrong VM");
    assert!(matches!(
        err,
        CrossVmError::WrongVm {
            expected: ChainKind::Evm,
            found: ChainKind::CosmWasm,
        }
    ));
}

#[cfg(feature = "tron")]
#[tokio::test]
async fn transfer_funds_rejects_an_amount_past_the_u64_base_unit() {
    // Tron (like Solana) carries balances as `u64`; the VM-agnostic `u128` amount must not be
    // silently truncated into it.
    let chain = TronChain::from(TRON_NILE.mock(test_wallets()));
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    let too_big = u128::from(u64::MAX) + 1;

    let err = AnyChain::from(chain)
        .transfer_funds(
            &Account::from(bob),
            TRON_NILE.native_symbol,
            too_big,
            TEST_WALLETS.alice,
        )
        .await
        .expect_err("an amount past u64::MAX cannot be represented in sun");
    let msg = err.to_string();
    assert!(
        msg.contains(&too_big.to_string()),
        "unexpected error: {msg}"
    );
    assert!(msg.contains("sun"), "unexpected error: {msg}");
}

mod hooks {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::prelude::*;
    use cross_vm_solidity::{Bytes, EvmGas, B256};

    fn empty_wallets() -> Rc<WalletFactory> {
        Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
    }

    fn base() -> ContractBase {
        let wallets = empty_wallets();
        ContractBase::new(AnyChain::from(ETHEREUM.mock(wallets)))
    }

    /// An empty EVM response costed the way the mock backend costs one: metered gas, no fee.
    fn evm_resp() -> AppResponse<()> {
        let gas = EvmGas {
            used: 21_000,
            fee: None,
        };
        AppResponse::evm((), Bytes::new(), vec![], B256::ZERO, gas)
    }

    #[test]
    fn after_hook_fires_with_label_and_kind() {
        let base = base();
        let seen: Rc<RefCell<Vec<(String, ChainKind)>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        base.on_after(move |ctx| {
            sink.borrow_mut()
                .push((ctx.label().to_string(), ctx.kind()));
            Ok(())
        });

        let resp = base.run_after("increment", evm_resp()).expect("after ok");
        assert_eq!(resp.kind(), ChainKind::Evm);
        assert_eq!(
            *seen.borrow(),
            vec![("increment".to_string(), ChainKind::Evm)]
        );
    }

    #[test]
    fn after_hook_reads_cost() {
        let base = base();
        let seen: Rc<RefCell<Option<Cost>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&seen);
        base.on_after(move |ctx| {
            *sink.borrow_mut() = ctx.cost();
            Ok(())
        });

        base.run_after("increment", evm_resp()).expect("after ok");
        assert_eq!(
            *seen.borrow(),
            Some(Cost {
                units: 21_000,
                unit: CostUnit::Gas,
                bandwidth: None,
                fee: None,
            })
        );
    }

    #[test]
    fn before_hook_err_aborts() {
        let base = base();
        base.on_before(|_| {
            Err(CrossVmError::Other {
                kind: ChainKind::Evm,
                reason: "vetoed".into(),
            })
        });
        let err = base.run_before("increment").unwrap_err();
        assert!(matches!(err, CrossVmError::Other { .. }));
    }

    #[test]
    fn after_hook_err_propagates() {
        let base = base();
        base.on_after(|_| {
            Err(CrossVmError::Other {
                kind: ChainKind::Evm,
                reason: "indexer down".into(),
            })
        });
        let res = base.run_after("increment", evm_resp());
        assert!(matches!(res, Err(CrossVmError::Other { .. })));
    }

    #[test]
    fn hooks_fire_in_registration_order() {
        let base = base();
        let order: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        for tag in [1u8, 2, 3] {
            let sink = Rc::clone(&order);
            base.on_after(move |_| {
                sink.borrow_mut().push(tag);
                Ok(())
            });
        }
        base.run_after("increment", evm_resp()).expect("after ok");
        assert_eq!(*order.borrow(), vec![1, 2, 3]);
    }

    #[test]
    fn no_hooks_is_a_noop() {
        let base = base();
        assert!(base.run_before("increment").is_ok());
        assert!(base.run_after("increment", evm_resp()).is_ok());
    }
}
