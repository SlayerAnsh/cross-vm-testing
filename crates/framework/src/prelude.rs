//! Common imports for writing cross-VM tests and scripts.

pub use crate::{
    Account, AnyChain, AppResponse, BeforeContext, ContractBase, Cost, CostUnit, EmptyWallets,
    EnvError, FundTarget, HookContext, MultiChainEnv, RawResponse, Running, Setup, Shortfall,
    TestWallets, EMPTY_WALLETS, TEST_WALLETS,
};
// Property-testing harness.
pub use crate::harness::{
    classify, decode_json_op, op_label, random_seed, sub_seed, AdvanceFn, CheckOutcome, ConfigOps,
    Coverage, Ctx, DecodeFn, DynInvariant, DynOp, DynOperation, Endurance, EnduranceConfig,
    EnduranceRunner, Expectation, Failure, FailureKind, Fuzz, FuzzRunner, GenerateFn, Harness,
    HarnessError, InvCoverage, Invariant, InvariantRunner, KindMix, OpDef, OpFuture, OpSetHarness,
    OpStat, Prng, RunMode, RunReport, Runner, Scenario, ScenarioRunner, ScenarioStep, Sequential,
    Stats, Verdict, Violation, WeightFn, DEFAULT_SHRINK_LIMIT,
};
pub use cross_vm_core::{
    ChainKind, ChainProvider, ChainSpec, CrossVmError, WalletFactory, WalletLabel, WalletSource,
    WalletSpec,
};

// Wallet roster macro and contract wrapper macro.
pub use cross_vm_macros::{cross_vm_contract, define_wallet_roster};
// Per-mode runner attribute macros (from the standalone harness crate).
pub use harness_core_macros::{endurance_runner, fuzz_runner, invariant_runner};

// CosmWasm
// Preset chain instances are internal-testing conveniences, gated behind the opt-in `presets`
// feature so a default `use cross_vm_framework::prelude::*;` does not pull them into scope.
#[cfg(all(feature = "cw", feature = "presets"))]
pub use cross_vm_cosmwasm::chains::{COSMOS_HUB, JUNO, LOCAL as CW_LOCAL, NEUTRON, OSMOSIS};
#[cfg(feature = "cw")]
pub use cross_vm_cosmwasm::{
    CwAsset, CwChain, CwContract, CwInterface, CwMockProvider, CwRpcProvider, CwSerde,
};

// EVM
#[cfg(all(feature = "evm", feature = "presets"))]
pub use cross_vm_solidity::chains::{
    ARBITRUM, BASE, ETHEREUM, LOCAL as EVM_LOCAL, OPTIMISM, POLYGON,
};
#[cfg(feature = "evm")]
pub use cross_vm_solidity::{
    EvmAsset, EvmChain, EvmExecution, EvmMockProvider, EvmRpcProvider, Log,
};

// Solana
#[cfg(all(feature = "solana", feature = "presets"))]
pub use cross_vm_solana::chains::{SOLANA_DEVNET, SOLANA_LOCALNET, SOLANA_MAINNET, SOLANA_TESTNET};
#[cfg(feature = "solana")]
pub use cross_vm_solana::{SvmAsset, SvmChain, SvmMockProvider, SvmRpcProvider};

// Tron
#[cfg(all(feature = "tron", feature = "presets"))]
pub use cross_vm_tron::chains::{
    LOCAL as TRON_LOCAL, MAINNET as TRON_MAINNET, NILE as TRON_NILE, SHASTA as TRON_SHASTA,
};
#[cfg(feature = "tron")]
pub use cross_vm_tron::{
    TronAddress, TronAsset, TronChain, TronExecution, TronMockProvider, TronRpcProvider,
};
