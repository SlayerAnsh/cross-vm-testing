//! Tests for the cross-VM testing environment.

use std::rc::Rc;

use crate::prelude::*;

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
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

mod hooks {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::prelude::*;
    use cross_vm_solidity::Bytes;

    fn empty_wallets() -> Rc<WalletFactory> {
        Rc::new(WalletFactory::from_roster(EmptyWallets::SPECS).expect("empty roster"))
    }

    fn base() -> ContractBase {
        let wallets = empty_wallets();
        ContractBase::new(AnyChain::from(ETHEREUM.mock(wallets)))
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

        let resp = base
            .run_after(
                "increment",
                AppResponse::evm((), Bytes::new(), vec![], None),
            )
            .expect("after ok");
        assert_eq!(resp.kind(), ChainKind::Evm);
        assert_eq!(
            *seen.borrow(),
            vec![("increment".to_string(), ChainKind::Evm)]
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
        let res = base.run_after(
            "increment",
            AppResponse::evm((), Bytes::new(), vec![], None),
        );
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
        base.run_after(
            "increment",
            AppResponse::evm((), Bytes::new(), vec![], None),
        )
        .expect("after ok");
        assert_eq!(*order.borrow(), vec![1, 2, 3]);
    }

    #[test]
    fn no_hooks_is_a_noop() {
        let base = base();
        assert!(base.run_before("increment").is_ok());
        assert!(base
            .run_after(
                "increment",
                AppResponse::evm((), Bytes::new(), vec![], None)
            )
            .is_ok());
    }
}
