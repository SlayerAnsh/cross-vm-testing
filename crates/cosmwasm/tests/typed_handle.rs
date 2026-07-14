//! `CwContract` handle: bind a `CwChain` + address once, then `execute` / `query` without
//! repeating the address. Drives the canonical counter contract on a mock chain.

use std::rc::Rc;

use cosmwasm_std::Empty;
use counter::{CountResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use cross_vm_core::WalletFactory;
use cross_vm_cosmwasm::chains::OSMOSIS;
use cross_vm_cosmwasm::{CwChain, CwContract, CwError, CwGasLimit};
use cross_vm_macros::define_wallet_roster;
use cw_multi_test::{Contract, ContractWrapper};

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

fn wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[tokio::test]
async fn handle_binds_address_for_execute_and_query() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    let stored = chain
        .store_code(
            counter_contract(),
            TEST_WALLETS.alice,
            CwGasLimit::Estimated,
        )
        .await
        .expect("store");
    let instantiated = chain
        .instantiate(
            stored.code_id,
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
            CwGasLimit::Estimated,
        )
        .await
        .expect("instantiate");

    let contract = chain.contract(instantiated.address);

    let res: CountResponse = contract.query(QueryMsg::GetCount {}).await.expect("query");
    assert_eq!(res.count, 0);

    contract
        .execute(ExecuteMsg::Increment {}, "alice", CwGasLimit::Estimated)
        .await
        .expect("increment");

    let res: CountResponse = contract.query(QueryMsg::GetCount {}).await.expect("query");
    assert_eq!(res.count, 1);
}

#[tokio::test]
async fn lifecycle_handle_exposes_the_deploy_tx_hashes() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    let contract = CwContract::<()>::new(chain.clone())
        .store_code(
            counter_contract(),
            TEST_WALLETS.alice,
            CwGasLimit::Estimated,
        )
        .await
        .expect("store")
        .instantiate(
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
            CwGasLimit::Estimated,
        )
        .await
        .expect("instantiate");

    let store_hash = contract.store_code_tx_hash().expect("stored");
    let init_hash = contract.instantiate_tx_hash().expect("instantiated");
    assert_eq!(store_hash.len(), 64);
    assert_eq!(init_hash.len(), 64);
    assert_ne!(store_hash, init_hash, "two transactions, two hashes");

    // A handle bound to an already-deployed address ran neither step, so it carries no hashes.
    let bound = chain.contract(contract.address().expect("instantiated").clone());
    assert!(bound.store_code_tx_hash().is_none());
    assert!(bound.instantiate_tx_hash().is_none());
}

/// The handle can forecast every lifecycle step it can run, gated by the same state as the step
/// itself: estimating an upload needs nothing, estimating an instantiate needs the stored
/// `code_id`, estimating an execute needs the bound address. On the mock every reachable
/// forecast is `None` (no gas meter), and an unreachable one is the sibling op's `CwError`.
#[tokio::test]
async fn handle_estimators_mirror_the_lifecycle_gating() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    // A fresh handle can estimate an upload before any step has run: no state needed.
    let fresh = CwContract::<()>::new(chain.clone());
    let est = fresh
        .estimate_store_code(counter_contract(), TEST_WALLETS.alice)
        .await
        .expect("estimate store_code");
    assert!(est.is_none(), "mock cannot meter, got {est:?}");

    // Estimating a step whose prerequisite has not run fails like the step itself would.
    let err = fresh
        .estimate_instantiate(InstantiateMsg {}, TEST_WALLETS.alice, &[], "counter")
        .await
        .expect_err("no code_id stored yet");
    assert!(matches!(err, CwError::Deploy(_)), "got {err:?}");
    let err = fresh
        .estimate_execute(ExecuteMsg::Increment {}, "alice")
        .await
        .expect_err("no address bound yet");
    assert!(matches!(err, CwError::Execute(_)), "got {err:?}");

    // Once the prerequisite has run, the forecast is reachable; on the mock it stays absent,
    // mirroring the `gas` field of the receipt it forecasts.
    let stored = fresh
        .store_code(
            counter_contract(),
            TEST_WALLETS.alice,
            CwGasLimit::Estimated,
        )
        .await
        .expect("store");
    let est = stored
        .estimate_instantiate(InstantiateMsg {}, TEST_WALLETS.alice, &[], "counter")
        .await
        .expect("estimate instantiate");
    assert!(est.is_none(), "mock cannot meter, got {est:?}");

    let contract = stored
        .instantiate(
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
            CwGasLimit::Estimated,
        )
        .await
        .expect("instantiate");
    let est = contract
        .estimate_execute(ExecuteMsg::Increment {}, "alice")
        .await
        .expect("estimate execute");
    assert!(est.is_none(), "mock cannot meter, got {est:?}");
    let est = contract
        .estimate_execute_with_funds(ExecuteMsg::Increment {}, "alice", &[])
        .await
        .expect("estimate execute_with_funds");
    assert!(est.is_none(), "mock cannot meter, got {est:?}");
}

#[tokio::test]
async fn lifecycle_handle_exposes_the_deploy_gas() {
    let chain: CwChain = OSMOSIS.mock(wallets()).into();

    let contract = CwContract::<()>::new(chain.clone())
        .store_code(
            counter_contract(),
            TEST_WALLETS.alice,
            CwGasLimit::Estimated,
        )
        .await
        .expect("store")
        .instantiate(
            InstantiateMsg {},
            TEST_WALLETS.alice,
            &[],
            "counter",
            CwGasLimit::Estimated,
        )
        .await
        .expect("instantiate");

    // Both deploy steps ran (outer `Some`), on a backend with no gas meter (inner `None`). The
    // handle must not flatten the two: `Some(None)` is a step that ran and could not be priced.
    assert_eq!(contract.store_code_gas(), Some(None));
    assert_eq!(contract.instantiate_gas(), Some(None));

    // A handle bound to an already-deployed address ran neither step, so there is no cost to
    // report at all. This is the absence the mock's `Some(None)` must stay distinguishable from:
    // "never uploaded" is not "uploaded for free".
    let bound = chain.contract(contract.address().expect("instantiated").clone());
    assert_eq!(bound.store_code_gas(), None);
    assert_eq!(bound.instantiate_gas(), None);
}
