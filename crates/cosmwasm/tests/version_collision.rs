//! Proof that identically-named typed methods on two different contracts no longer collide.
//!
//! Both `counter` and `vault` now expose `set_version` / `get_version` via their `CwExecuteFns`
//! / `CwQueryFns` derives. The derives emit associated-type-scoped impls
//! (`impl<I: CwInterface<ExecuteMsg = ThisEnum>> ExecuteMsgFns for CwContract<I>`), so a
//! `CwContract<CounterContract>` and a `CwContract<VaultContract>` each resolve the shared method
//! name to their own impl — no ambiguity even with all four traits in scope at once.
//!
//! The traits are imported `as _`: that brings them into scope for method resolution (so
//! `set_version` / `get_version` are callable) without binding the trait *names*, which would
//! otherwise collide since both crates export `ExecuteMsgFns` and `QueryMsgFns`.

use std::rc::Rc;

use cosmwasm_std::Empty;
use cross_vm_core::WalletFactory;
use cross_vm_cosmwasm::chains::OSMOSIS;
use cross_vm_cosmwasm::CwChain;
use cross_vm_macros::define_wallet_roster;
use cw_multi_test::{Contract, ContractWrapper};

use counter::{ExecuteMsgFns as _, QueryMsgFns as _};
use vault::{ExecuteMsgFns as _, QueryMsgFns as _};

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
    };
}

fn counter_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        counter::execute,
        counter::instantiate,
        counter::query,
    ))
}

fn vault_contract() -> Box<dyn Contract<Empty, Empty>> {
    Box::new(ContractWrapper::new(
        vault::execute,
        vault::instantiate,
        vault::query,
    ))
}

fn wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[tokio::test]
async fn shared_method_names_resolve_per_contract() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    let counter_code = chain
        .store_code(counter_contract(), TEST_WALLETS.alice)
        .await
        .expect("store counter");
    let counter_addr = chain
        .instantiate(
            counter_code,
            counter::InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
        )
        .await
        .expect("instantiate counter");

    let vault_code = chain
        .store_code(vault_contract(), TEST_WALLETS.alice)
        .await
        .expect("store vault");
    let vault_addr = chain
        .instantiate(
            vault_code,
            vault::InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "vault",
        )
        .await
        .expect("instantiate vault");

    // Two typed handles, distinguished only by their marker type. `set_version` / `get_version`
    // resolve to the counter impl vs the vault impl purely from the handle's `I`.
    let c = chain.contract_as::<counter::CounterContract>(counter_addr);
    let v = chain.contract_as::<vault::VaultContract>(vault_addr);

    c.set_version("alice", 7)
        .await
        .expect("counter set_version");
    v.set_version("alice", 9).await.expect("vault set_version");

    assert_eq!(
        c.get_version().await.expect("counter get_version").version,
        7
    );
    assert_eq!(v.get_version().await.expect("vault get_version").version, 9);
}
