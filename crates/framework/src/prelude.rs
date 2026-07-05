//! Common imports for writing cross-VM tests and scripts.

pub use crate::{
    Account, AnyChain, AppResponse, BeforeContext, ContractBase, EmptyWallets, EnvError,
    FundTarget, HookContext, MultiChainEnv, RawResponse, Running, Setup, Shortfall, TestWallets,
    EMPTY_WALLETS, TEST_WALLETS,
};
// Property-testing harness.
pub use crate::harness::{
    classify, op_label, random_seed, sub_seed, CheckOutcome, Coverage, Ctx, Endurance,
    EnduranceConfig, EnduranceRunner, Expectation, Failure, FailureKind, Fuzz, FuzzRunner, Harness,
    HarnessError, InvCoverage, Invariant, InvariantRunner, KindMix, OpStat, Prng, RunMode,
    RunReport, Runner, Scenario, ScenarioRunner, ScenarioStep, Sequential, Stats, Verdict,
    Violation, DEFAULT_SHRINK_LIMIT,
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
#[cfg(feature = "cw")]
pub use cross_vm_cosmwasm::chains::{COSMOS_HUB, JUNO, LOCAL as CW_LOCAL, NEUTRON, OSMOSIS};
#[cfg(feature = "cw")]
pub use cross_vm_cosmwasm::{
    CwAsset, CwChain, CwContract, CwInterface, CwMockProvider, CwRpcProvider, CwSerde,
};

// EVM
#[cfg(feature = "evm")]
pub use cross_vm_solidity::chains::{
    ARBITRUM, BASE, ETHEREUM, LOCAL as EVM_LOCAL, OPTIMISM, POLYGON,
};
#[cfg(feature = "evm")]
pub use cross_vm_solidity::{
    EvmAsset, EvmChain, EvmExecution, EvmMockProvider, EvmRpcProvider, Log,
};

// Solana
#[cfg(feature = "solana")]
pub use cross_vm_solana::chains::{SOLANA_DEVNET, SOLANA_LOCALNET, SOLANA_MAINNET, SOLANA_TESTNET};
#[cfg(feature = "solana")]
pub use cross_vm_solana::{SvmAsset, SvmChain, SvmMockProvider, SvmRpcProvider};

// Tron
#[cfg(feature = "tron")]
pub use cross_vm_tron::chains::{
    LOCAL as TRON_LOCAL, MAINNET as TRON_MAINNET, NILE as TRON_NILE, SHASTA as TRON_SHASTA,
};
#[cfg(feature = "tron")]
pub use cross_vm_tron::{
    TronAddress, TronAsset, TronChain, TronExecution, TronMockProvider, TronRpcProvider,
};
