//! Unit tests for the EVM provider.

use std::rc::Rc;

use crate::chains::{ETHEREUM, LOCAL};
use crate::EvmGasLimit;
use alloy_primitives::{Address, Bytes, B256, U256};
use cross_vm_core::{BlockTime, ChainProvider, ChainSpec, WalletFactory};
use cross_vm_macros::define_wallet_roster;

/// A limit generous enough for every transaction these tests submit, so a test that is not about
/// the limit does not accidentally become one.
const AMPLE: EvmGasLimit = EvmGasLimit::Exact(30_000_000);

define_wallet_roster! {
    pub const TEST_WALLETS: TestWallets = {
        alice: auto @ 0,
        bob: auto @ 1,
    };
}

fn empty_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(&[]).unwrap())
}

fn test_wallets() -> Rc<WalletFactory> {
    Rc::new(WalletFactory::from_roster(TestWallets::SPECS).expect("resolve roster"))
}

#[test]
fn predefined_chain_metadata() {
    assert_eq!(ETHEREUM.chain_id(), "1");
    assert_eq!(ETHEREUM.numeric_id(), 1);
    assert_eq!(ETHEREUM.native_symbol(), "ETH");
}

#[tokio::test]
async fn new_account_is_funded() {
    let mut chain = ETHEREUM.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.balance(&alice).await.unwrap(),
        U256::from(crate::DEFAULT_FUNDING_WEI)
    );
}

#[tokio::test]
async fn set_and_read_balance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;
    chain
        .set_balance(&bob, "ETH", U256::from(42u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(42u64));
}

#[tokio::test]
async fn set_balance_validates_denom() {
    let mut chain = LOCAL.mock(empty_wallets());
    let bob = chain.new_account("bob").await;

    // Unknown denom is rejected.
    assert!(chain
        .set_balance(&bob, "BTC", U256::from(1u64))
        .await
        .is_err());

    // The native symbol is accepted case-insensitively.
    chain
        .set_balance(&bob, "eth", U256::from(7u64))
        .await
        .unwrap();
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(7u64));
}

#[tokio::test]
async fn blocks_advance() {
    let mut chain = LOCAL.mock(empty_wallets());
    let h0 = chain.block_height().await;
    chain.advance_blocks(5, BlockTime::Increment(1)).await;
    assert_eq!(chain.block_height().await, h0 + 5);
}

#[tokio::test]
async fn reads_storage_slot_written_by_constructor() {
    // Initcode whose constructor writes 42 into storage slot 0, then returns an empty runtime:
    //   PUSH1 0x2a, PUSH1 0x00, SSTORE, PUSH1 0x00, PUSH1 0x00, RETURN.
    let initcode = Bytes::from(vec![
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
    ]);
    let mut chain = LOCAL.mock(empty_wallets());
    let deployer = chain.new_account("deployer").await;
    let deploy = chain
        .deploy_create(initcode, [], &deployer, AMPLE)
        .await
        .expect("storage-writing deploy succeeds");
    // The constructor wrote 42 at slot 0; an untouched slot reads as zero.
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::ZERO)
            .await
            .unwrap(),
        U256::from(42u64)
    );
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::from(1u64))
            .await
            .unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn mutating_ops_carry_a_transaction_hash() {
    // Initcode returning an empty runtime: enough to exercise deploy -> call on the mock.
    let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let deploy = chain
        .deploy_create(initcode, [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy");
    assert_ne!(deploy.address, Address::ZERO);
    assert_ne!(deploy.tx_hash, B256::ZERO);

    let exec = chain
        .call(&deploy.address, [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("call");
    assert_ne!(exec.tx_hash, B256::ZERO);
    // Deploys and calls draw from one hash sequence, so the two never collide.
    assert_ne!(exec.tx_hash, deploy.tx_hash);

    let exec_value = chain
        .call_value(
            &deploy.address,
            [],
            TEST_WALLETS.alice,
            U256::from(1u64),
            AMPLE,
        )
        .await
        .expect("payable call");
    assert_ne!(exec_value.tx_hash, B256::ZERO);
    assert_ne!(exec_value.tx_hash, exec.tx_hash);
}

#[tokio::test]
async fn mutating_ops_report_the_gas_they_burned() {
    // Initcode whose constructor writes 42 into slot 0 before returning an empty runtime: a
    // deploy that pays for an SSTORE plus the create intrinsic, against a call into the empty
    // runtime that pays for little beyond the call intrinsic.
    let initcode = Bytes::from(vec![
        0x60, 0x2a, 0x60, 0x00, 0x55, 0x60, 0x00, 0x60, 0x00, 0xf3,
    ]);
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let deploy = chain
        .deploy_create(initcode, [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy");
    assert!(deploy.gas.used > 0, "deploy reported no gas");

    let exec = chain
        .call(&deploy.address, [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("call");
    assert!(exec.gas.used > 0, "call reported no gas");
    assert!(
        deploy.gas.used > exec.gas.used,
        "deploy ({}) must cost more gas than a trivial call ({})",
        deploy.gas.used,
        exec.gas.used
    );

    // The mock has no gas price to multiply by, so it reports no fee rather than a fake zero.
    assert_eq!(deploy.gas.fee, None);
    assert_eq!(exec.gas.fee, None);
}

/// Initcode returning `runtime` (CODECOPY of the bytes trailing this 12-byte prologue).
fn initcode_returning(runtime: &[u8]) -> Bytes {
    let n = runtime.len() as u8;
    let mut code = vec![
        0x60, n, 0x60, 0x0c, 0x60, 0x00, 0x39, 0x60, n, 0x60, 0x00, 0xf3,
    ];
    code.extend_from_slice(runtime);
    Bytes::from(code)
}

/// A contract whose fallback writes 0x2a to slot 0: PUSH1 0x2a, PUSH1 0x00, SSTORE, STOP.
fn storing_contract() -> Bytes {
    initcode_returning(&[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00])
}

/// A contract whose fallback reverts with empty data: PUSH1 0x00, PUSH1 0x00, REVERT.
fn reverting_contract() -> Bytes {
    initcode_returning(&[0x60, 0x00, 0x60, 0x00, 0xfd])
}

/// A contract whose constructor writes 1 to slot 0 and whose fallback clears it again, so calling
/// it earns the EIP-3529 storage-clearing refund. The refund is deducted *after* execution, so the
/// call burns more gas than it is finally billed: the gap `Estimated` has to cover.
///
/// Constructor: PUSH1 0x01, PUSH1 0x00, SSTORE, then the 12-byte CODECOPY-return prologue (so the
/// runtime starts at offset 0x11). Runtime: PUSH1 0x00, PUSH1 0x00, SSTORE, STOP.
fn refunding_contract() -> Bytes {
    let runtime: &[u8] = &[0x60, 0x00, 0x60, 0x00, 0x55, 0x00];
    let n = runtime.len() as u8;
    let mut code = vec![
        0x60, 0x01, 0x60, 0x00, 0x55, // slot 0 = 1
        0x60, n, 0x60, 0x11, 0x60, 0x00, 0x39, 0x60, n, 0x60, 0x00, 0xf3,
    ];
    code.extend_from_slice(runtime);
    Bytes::from(code)
}

#[tokio::test]
async fn estimate_matches_the_gas_the_op_reports_when_executed() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    // Deploy: the estimate runs against the state (and nonce) the real deploy will run against, so
    // the two are the same transaction and the mock meters them identically.
    let estimated_deploy = chain
        .estimate_deploy_create(storing_contract(), [], TEST_WALLETS.alice)
        .await
        .expect("estimate deploy");
    let deploy = chain
        .deploy_create(storing_contract(), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy after estimating it");
    assert_eq!(estimated_deploy.used, deploy.gas.used);

    // Call: the estimated SSTORE is cold and 0 -> nonzero, exactly as the real one is.
    let estimated_call = chain
        .estimate_call(&deploy.address, [], TEST_WALLETS.alice)
        .await
        .expect("estimate call");
    let exec = chain
        .call(&deploy.address, [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("call after estimating it");
    assert_eq!(estimated_call.used, exec.gas.used);
    assert!(
        estimated_call.used > 21_000,
        "an SSTORE costs more than intrinsic gas, got {}",
        estimated_call.used
    );

    // Estimating committed nothing, so the op it forecast still runs: the SSTORE landed, and it
    // landed once (the estimate did not write it first, which would have made the real call warm
    // and cheaper than forecast).
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::ZERO)
            .await
            .unwrap(),
        U256::from(0x2au64)
    );

    // The mock has no gas price, so it forecasts no fee rather than a fake zero.
    assert_eq!(estimated_deploy.fee, None);
    assert_eq!(estimated_call.fee, None);
}

#[tokio::test]
async fn estimating_a_payable_call_matches_what_it_costs() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    let value = U256::from(5u64);

    // The mock mints the caller the funds a payable call needs; the estimate must do the same, or
    // it would fail where the call it forecasts succeeds.
    let estimated = chain
        .estimate_call_value(&bob, [], TEST_WALLETS.alice, value)
        .await
        .expect("estimate payable call");
    let exec = chain
        .call_value(&bob, [], TEST_WALLETS.alice, value, AMPLE)
        .await
        .expect("payable call after estimating it");
    assert_eq!(estimated.used, exec.gas.used);
    // The estimate's top-up was never committed: bob is paid once, by the real call alone.
    assert_eq!(chain.balance(&bob).await.unwrap(), value);
}

#[tokio::test]
async fn estimating_a_reverting_op_errors_rather_than_reporting_gas() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let target = chain
        .deploy_create(reverting_contract(), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy")
        .address;

    let err = chain
        .estimate_call(&target, [], TEST_WALLETS.alice)
        .await
        .expect_err("estimating a reverting call must error");
    assert!(
        err.to_string().contains("reverted"),
        "unexpected error: {err}"
    );

    // Initcode that reverts: the create never completes, so there is no gas figure to hand back.
    let err = chain
        .estimate_deploy_create(
            Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xfd]),
            [],
            TEST_WALLETS.alice,
        )
        .await
        .expect_err("estimating a reverting deploy must error");
    assert!(
        err.to_string().contains("reverted"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn an_exact_limit_is_honored_verbatim() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let target = chain
        .deploy_create(storing_contract(), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy")
        .address;
    let needed = chain
        .estimate_call(&target, [], TEST_WALLETS.alice)
        .await
        .expect("estimate")
        .used;

    // Exactly the cost, not a wei of headroom: the limit is a budget, not a fee, so the call fits
    // and is billed precisely what it was forecast.
    let exec = chain
        .call(&target, [], TEST_WALLETS.alice, EvmGasLimit::Exact(needed))
        .await
        .expect("a limit equal to the cost must succeed");
    assert_eq!(exec.gas.used, needed);
    assert_eq!(
        chain.get_storage_at(&target, U256::ZERO).await.unwrap(),
        U256::from(0x2au64)
    );
}

#[tokio::test]
async fn an_exact_limit_below_the_cost_runs_out_of_gas() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let target = chain
        .deploy_create(storing_contract(), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy")
        .address;
    let needed = chain
        .estimate_call(&target, [], TEST_WALLETS.alice)
        .await
        .expect("estimate")
        .used;

    // One gas short: past the intrinsic check, so the SSTORE exhausts the budget mid-flight. A
    // too-low `Exact` is not corrected upwards, which is the whole point of `Exact`: an
    // out-of-gas test must be expressible.
    let err = chain
        .call(
            &target,
            [],
            TEST_WALLETS.alice,
            EvmGasLimit::Exact(needed - 1),
        )
        .await
        .expect_err("a limit under the cost must fail");
    assert!(
        err.to_string().contains("OutOfGas"),
        "expected an out-of-gas failure, got: {err}"
    );
    assert_eq!(
        chain.get_storage_at(&target, U256::ZERO).await.unwrap(),
        U256::ZERO,
        "an out-of-gas call must commit nothing"
    );

    // Likewise on the create path.
    let err = chain
        .deploy_create(
            storing_contract(),
            [],
            TEST_WALLETS.alice,
            EvmGasLimit::Exact(21_000),
        )
        .await
        .expect_err("a deploy under its cost must fail");
    assert!(!err.to_string().is_empty());
}

#[tokio::test]
async fn an_estimated_limit_lands_above_the_gas_the_op_reports() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let deploy = chain
        .deploy_create(
            storing_contract(),
            [],
            TEST_WALLETS.alice,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("an estimated deploy must fit under its own limit");
    let exec = chain
        .call(
            &deploy.address,
            [],
            TEST_WALLETS.alice,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("an estimated call must fit under its own limit");

    // `Estimated` submits `estimate * gas_adjustment`, so the limit it derives sits above the gas
    // the op is then billed. The estimate itself is unchanged by the multiplier (it is the cost,
    // not the budget), so the billed figure is the estimate and the limit is strictly more.
    let info = chain.chain_info();
    for used in [deploy.gas.used, exec.gas.used] {
        assert!(
            info.adjusted_gas_limit(used) > used,
            "the adjusted limit ({}) must exceed the gas the op burned ({used})",
            info.adjusted_gas_limit(used)
        );
    }
    assert_eq!(info.gas_adjustment, 1.3);
    assert_eq!(info.adjusted_gas_limit(100_000), 130_000);

    // The estimate committed nothing, so the op it forecast still did the work.
    assert_eq!(
        chain
            .get_storage_at(&deploy.address, U256::ZERO)
            .await
            .unwrap(),
        U256::from(0x2au64)
    );
}

#[tokio::test]
async fn an_estimated_limit_covers_a_refund_an_exact_estimate_would_not() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let target = chain
        .deploy_create(refunding_contract(), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy")
        .address;

    // Clearing the slot earns a refund, and an estimate reports the *billed* gas, already net of
    // it. So the estimate alone is not a sufficient limit: the call burns the pre-refund figure
    // before the refund is ever credited.
    let quote = chain
        .estimate_call(&target, [], TEST_WALLETS.alice)
        .await
        .expect("estimate")
        .used;
    let err = chain
        .call(&target, [], TEST_WALLETS.alice, EvmGasLimit::Exact(quote))
        .await
        .expect_err("a refunding call cannot run under a limit equal to its billed gas");
    assert!(
        err.to_string().contains("OutOfGas"),
        "expected an out-of-gas failure, got: {err}"
    );

    // `gas_adjustment` is exactly what closes that gap: the refund is capped at a fifth of the gas
    // burned, so 1.3x the billed figure always covers the burn.
    let exec = chain
        .call(&target, [], TEST_WALLETS.alice, EvmGasLimit::Estimated)
        .await
        .expect("an estimated limit must cover the refund the estimate hides");
    assert_eq!(exec.gas.used, quote, "the refund does not move the bill");
    assert_eq!(
        chain.get_storage_at(&target, U256::ZERO).await.unwrap(),
        U256::ZERO,
        "the call cleared the slot"
    );
}

#[tokio::test]
async fn get_storage_at_plumbs_through_chain() {
    // Exercise the `EvmChain` enum dispatch: an unset slot reads as zero.
    let mut chain = crate::EvmChain::from(LOCAL.mock(empty_wallets()));
    let alice = chain.new_account("alice").await;
    assert_eq!(
        chain.get_storage_at(&alice, U256::ZERO).await.unwrap(),
        U256::ZERO
    );
}

#[tokio::test]
async fn get_code_returns_runtime_for_a_contract_and_empty_for_an_eoa() {
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();

    // An account with no deployed code (an EOA) reads back empty.
    assert!(
        chain.get_code(&alice).await.unwrap().is_empty(),
        "an EOA carries no code"
    );

    // A contract whose runtime is a known byte string reads that string back verbatim.
    let runtime: &[u8] = &[0x60, 0x2a, 0x60, 0x00, 0x55, 0x00];
    let deploy = chain
        .deploy_create(initcode_returning(runtime), [], TEST_WALLETS.alice, AMPLE)
        .await
        .expect("deploy");
    assert_eq!(
        chain.get_code(&deploy.address).await.unwrap().as_ref(),
        runtime,
        "get_code must return the deployed runtime bytecode"
    );
}

#[tokio::test]
async fn mock_rejects_the_rpc_only_escape_hatches() {
    // The in-process mock has no node, no signer over a real transaction, and no mempool, so each
    // of these surfaces `unimplemented` rather than a fabricated answer.
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));

    let err = chain
        .raw_request("eth_blockNumber", serde_json::Value::Null)
        .await
        .expect_err("mock has no node to answer a raw request");
    assert!(err.to_string().contains("unimplemented"), "got: {err}");

    let err = chain
        .sign_transaction(
            alloy::rpc::types::TransactionRequest::default(),
            TEST_WALLETS.alice,
        )
        .await
        .expect_err("mock signs no real transaction");
    assert!(err.to_string().contains("unimplemented"), "got: {err}");

    let err = chain
        .send_raw_transaction(&[0x01])
        .await
        .expect_err("mock has no mempool");
    assert!(err.to_string().contains("unimplemented"), "got: {err}");
}

#[tokio::test]
async fn transfer_funds_moves_the_balance() {
    let mut chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "ETH", U256::from(1_000u64))
        .await
        .unwrap();

    let hash = chain
        .transfer_funds(&bob, "ETH", U256::from(400u64), TEST_WALLETS.alice, AMPLE)
        .await
        .expect("transfer");

    assert_eq!(chain.balance(&bob).await.unwrap(), U256::from(400u64));
    assert_eq!(chain.balance(&alice).await.unwrap(), U256::from(600u64));
    // The mock's synthetic hash is rendered like the live one: 0x + 32 bytes of lowercase hex.
    assert!(hash.starts_with("0x"), "hash `{hash}` is not 0x-prefixed");
    assert_eq!(hash.len(), 66, "hash `{hash}` is not 32 bytes of hex");
    assert!(hash[2..]
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[tokio::test]
async fn transfer_funds_rejects_unknown_denom() {
    let mut chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let alice = chain.wallet_address(TEST_WALLETS.alice).await.unwrap();
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();
    chain
        .set_balance(&alice, "ETH", U256::from(1_000u64))
        .await
        .unwrap();

    let err = chain
        .transfer_funds(&bob, "BTC", U256::from(1u64), TEST_WALLETS.alice, AMPLE)
        .await
        .expect_err("unknown denom is rejected");
    assert!(
        err.to_string().contains("unknown denom 'BTC'"),
        "unexpected error: {err}"
    );
    // The rejected transfer moved nothing.
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::ZERO);
}

#[tokio::test]
async fn transfer_funds_rejects_insufficient_balance() {
    // The mock mints on a payable `call_value`; a transfer must not, so a short sender errors.
    let chain = crate::EvmChain::from(LOCAL.mock(test_wallets()));
    let bob = chain.wallet_address(TEST_WALLETS.bob).await.unwrap();

    let err = chain
        .transfer_funds(&bob, "ETH", U256::from(1u64), TEST_WALLETS.alice, AMPLE)
        .await
        .expect_err("an unfunded sender cannot transfer");
    assert!(
        err.to_string().contains("insufficient funds"),
        "unexpected error: {err}"
    );
    assert_eq!(chain.balance(&bob).await.unwrap(), U256::ZERO);
}

/// The EIP-55 spec's own published test vectors: two all-caps, two all-lower, four normal.
/// Each is already in its checksummed form, so re-checksumming a lowercased copy must reproduce it.
const EIP55_VECTORS: [&str; 8] = [
    // All caps
    "0x52908400098527886E0F7030069857D2E4169EE7",
    "0x8617E340B3D01FA5F11F306F4090FD50E238070D",
    // All lower
    "0xde709f2102306220921060314715629080e2fb77",
    "0x27b1fdb04752bbc536007a920d24acb045561c26",
    // Normal
    "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
    "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
    "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
    "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
];

#[test]
fn address_rendering_matches_eip55_reference_vectors() {
    for vector in EIP55_VECTORS {
        // Parse the *lowercased* form, so the rendered case can only come from a checksum
        // computation and not from echoing back the input's case.
        let addr: Address = vector
            .to_lowercase()
            .parse()
            .expect("EIP-55 vector is valid hex");
        assert_eq!(
            addr.to_string(),
            vector,
            "rendering `{vector}` lost its EIP-55 checksum"
        );
    }
}

#[tokio::test]
async fn chain_ops_render_eip55_checksummed_addresses() {
    // Addresses out of the mock are deterministic: an account is keccak(label)[12..] and a
    // deploy is CREATE(deployer, nonce 0). The literals below were checksummed with an
    // implementation independent of alloy, so matching them pins the rendered case, not just
    // the bytes. Both are mixed-case, so any lowercasing regression fails this test.
    const ALICE: &str = "0x5dad7600C5D89fE3824fFa99ec1c3eB8BF3b0501";
    const DEPLOYER: &str = "0x1b5CEb79b60DC455aD691D856E6E4025Cf542CAA";
    const CONTRACT: &str = "0x25A25a4Cd120784f7428d26001d9E34FFb90FAFe";
    for pinned in [ALICE, DEPLOYER, CONTRACT] {
        assert_ne!(
            pinned,
            pinned.to_lowercase(),
            "`{pinned}` must be mixed-case or it cannot detect a lowercasing regression"
        );
    }

    let mut chain = LOCAL.mock(empty_wallets());
    let alice = chain.new_account("alice").await;
    assert_eq!(alice.to_string(), ALICE);

    let deployer = chain.new_account("deployer").await;
    assert_eq!(deployer.to_string(), DEPLOYER);

    // Initcode returning an empty runtime.
    let initcode = Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xf3]);
    let deploy = chain
        .deploy_create(initcode, [], &deployer, AMPLE)
        .await
        .expect("deploy");
    assert_eq!(deploy.address.to_string(), CONTRACT);
}

#[tokio::test]
async fn rpc_write_paths_unimplemented() {
    let mut chain = ETHEREUM.rpc(empty_wallets());
    assert!(chain
        .set_balance(&alloy_primitives::Address::ZERO, "ETH", U256::from(1u64))
        .await
        .is_err());
}
