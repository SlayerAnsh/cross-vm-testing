//! Gas limits: what `Exact` and `Estimated` actually do at a call site.
//!
//! Both tests here run against a mock that genuinely meters and genuinely enforces, so a limit
//! under the true cost really does fail. That is the whole point of the file, and it is why the
//! other two VMs are absent rather than covered for symmetry:
//!
//! - The CosmWasm mock has no gas meter (`cw-multi-test` does not bill), so a `CwGasLimit` is
//!   inert there and a `CwGasLimit::Exact(1)` would *succeed*. A test asserting an out-of-gas
//!   failure on it could only pass by asserting nothing, which is worse than not testing it.
//! - Solana's budget is enforced by aborting the transaction rather than by an out-of-gas halt, a
//!   different failure with a different meaning; it does not belong in a shared assertion with
//!   these two.
//!
//! The limit under test is derived from the estimator each time, never hardcoded: a hand-picked
//! constant would silently stop being "one gas short of the true cost" the moment the contract or
//! the EVM's pricing changed, and the test would keep passing for the wrong reason.

use cross_vm_framework::prelude::*;
use cross_vm_solidity::Bytes;

use alloy::sol_types::SolCall;
use cross_vm_common::mocks::counter::{evm as evm_counter, tron as tron_counter};

use crate::support::{fund_alice, test_wallets};

/// The energy policy the Tron deploy below carries. The mock ignores it, but a deploy must state
/// one, and this test is not about how the deployed contract bills its future callers.
const CALLER_PAYS: TronEnergyPolicy = TronEnergyPolicy {
    consume_user_resource_percent: 100,
    origin_energy_limit: 0,
};

fn increment_calldata() -> Bytes {
    Bytes::from(evm_counter::Counter::incrementCall {}.abi_encode())
}

fn count_calldata() -> Bytes {
    Bytes::from(evm_counter::Counter::countCall {}.abi_encode())
}

fn decode_count(out: &[u8]) -> u64 {
    evm_counter::Counter::countCall::abi_decode_returns(out)
        .expect("decode count")
        .saturating_to::<u64>()
}

#[tokio::test]
async fn evm_exact_limit_below_the_true_cost_runs_out_of_gas() {
    let mut chain = AnyChain::from(ETHEREUM.mock(test_wallets()));
    fund_alice(&mut chain).await;
    let AnyChain::Evm(evm) = &chain else {
        unreachable!("built an EVM mock")
    };

    let counter = evm
        .deploy_create(
            evm_counter::Counter::BYTECODE.clone(),
            Bytes::new(),
            TEST_WALLETS.alice,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("deploy under an estimated limit")
        .address;

    // What the next increment is forecast to burn, against the state it is about to run on.
    let forecast = evm
        .estimate_call(&counter, increment_calldata(), TEST_WALLETS.alice)
        .await
        .expect("estimate increment");
    assert!(
        forecast.used > 21_000,
        "an increment must cost more than a bare transfer, got {}",
        forecast.used
    );

    // One gas short: past the intrinsic check, so the EVM starts executing and the counter's
    // SSTORE exhausts the budget mid-flight. `Exact` is honored as a chain honors it, so the
    // too-low limit is submitted rather than corrected.
    let err = evm
        .call(
            &counter,
            increment_calldata(),
            TEST_WALLETS.alice,
            EvmGasLimit::Exact(forecast.used - 1),
        )
        .await
        .expect_err("a limit under the true cost must fail");
    assert!(
        err.to_string().contains("OutOfGas"),
        "must fail out of gas, got: {err}"
    );

    // An out-of-gas call commits nothing: the increment it was running died with it.
    let out = evm
        .static_call(&counter, count_calldata())
        .await
        .expect("read count");
    assert_eq!(
        decode_count(&out),
        0,
        "an out-of-gas call must commit nothing"
    );

    // The same call under `Estimated` lands, and is billed exactly what it was forecast: the
    // estimate is a measurement of this op against this state, not a guess. The limit the caller
    // never had to name sat above it, by the chain's `gas_adjustment`.
    let exec = evm
        .call(
            &counter,
            increment_calldata(),
            TEST_WALLETS.alice,
            EvmGasLimit::Estimated,
        )
        .await
        .expect("an estimated limit must cover the op it estimated");
    assert_eq!(exec.gas.used, forecast.used);

    let out = evm
        .static_call(&counter, count_calldata())
        .await
        .expect("read count");
    assert_eq!(decode_count(&out), 1);
}

#[tokio::test]
async fn tron_exact_limit_below_the_true_cost_runs_out_of_gas() {
    let mut chain = AnyChain::from(TRON_LOCAL.mock(test_wallets()));
    fund_alice(&mut chain).await;
    let AnyChain::Tron(tron) = &chain else {
        unreachable!("built a Tron mock")
    };

    let counter = tron
        .deploy_create(
            tron_counter::Counter::BYTECODE.clone(),
            Bytes::new(),
            TEST_WALLETS.alice,
            TronLimit::Estimated,
            CALLER_PAYS,
        )
        .await
        .expect("deploy under an estimated limit")
        .address;

    let forecast: Cost = tron
        .estimate_call(&counter, increment_calldata(), TEST_WALLETS.alice)
        .await
        .expect("estimate increment")
        .into();
    // The mock is `revm`, so the quantity it forecasts is EVM gas, never Tron energy. `Gas` below
    // is the same unit this figure is denominated in, which is what makes the subtraction sound.
    assert_eq!(forecast.unit, CostUnit::Gas);
    let needed = u64::try_from(forecast.units).expect("mock gas fits a u64");

    let err = tron
        .call(
            &counter,
            increment_calldata(),
            TEST_WALLETS.alice,
            TronLimit::Gas(needed - 1),
        )
        .await
        .expect_err("a limit under the true cost must fail");
    assert!(
        err.to_string().contains("OutOfGas"),
        "must fail out of gas, got: {err}"
    );

    let out = tron
        .static_call(&counter, count_calldata())
        .await
        .expect("read count");
    assert_eq!(
        decode_count(&out),
        0,
        "an out-of-gas call must commit nothing"
    );

    // `Fee` is a sun ceiling, which only java-tron can price into energy. Offering one to the mock
    // is a unit error, and it is rejected rather than reinterpreted as a gas budget: `needed` sun
    // and `needed` gas are not the same quantity, and silently treating them as one is exactly the
    // bug the tagged variants exist to prevent.
    let err = tron
        .call(
            &counter,
            increment_calldata(),
            TEST_WALLETS.alice,
            TronLimit::Fee(1_000_000_000),
        )
        .await
        .expect_err("a fee limit cannot bound a revm transaction");
    assert!(
        err.to_string().contains("TronLimit::Gas"),
        "the error must name the limit the mock can honor, got: {err}"
    );

    // `Estimated` is the one variant both backends resolve, each in the unit it can meter.
    let exec = tron
        .call(
            &counter,
            increment_calldata(),
            TEST_WALLETS.alice,
            TronLimit::Estimated,
        )
        .await
        .expect("an estimated limit must cover the op it estimated");
    assert_eq!(Cost::from(exec.resources).units, u128::from(needed));

    let out = tron
        .static_call(&counter, count_calldata())
        .await
        .expect("read count");
    assert_eq!(decode_count(&out), 1);
}
