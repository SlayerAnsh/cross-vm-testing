//! Predefined EVM chains.
//!
//! Each constant is an [`EvmChainInfo`]. Use them as `EvmMockProvider::new(ETHEREUM)`
//! or `ETHEREUM.mock()`.

use cross_vm_core::{ChainKind, ChainSpec};
use revm::primitives::hardfork::SpecId;

/// Metadata describing an EVM chain.
#[derive(Debug, Clone, Copy)]
pub struct EvmChainInfo {
    /// EIP-155 chain id in string form (e.g. `"1"`); parsed to `u64` for the VM.
    pub chain_id: &'static str,
    /// Human-readable name, e.g. `"Ethereum"`.
    pub name: &'static str,
    /// Hardfork the mock VM executes against.
    pub spec_id: SpecId,
    /// Native token symbol, e.g. `"ETH"`.
    pub native_symbol: &'static str,
    /// Default public RPC endpoint, if known.
    pub rpc_url: Option<&'static str>,
}

impl EvmChainInfo {
    /// Numeric chain id used to configure the VM.
    pub fn numeric_id(&self) -> u64 {
        self.chain_id.parse().unwrap_or(1)
    }
}

impl ChainSpec for EvmChainInfo {
    fn chain_id(&self) -> &str {
        self.chain_id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn native_symbol(&self) -> &str {
        self.native_symbol
    }
    fn rpc_url(&self) -> Option<&str> {
        self.rpc_url
    }
    fn kind(&self) -> ChainKind {
        ChainKind::Evm
    }
}

/// Ethereum mainnet.
pub const ETHEREUM: EvmChainInfo = EvmChainInfo {
    chain_id: "1",
    name: "Ethereum",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    rpc_url: Some("https://eth.llamarpc.com"),
};

/// Arbitrum One.
pub const ARBITRUM: EvmChainInfo = EvmChainInfo {
    chain_id: "42161",
    name: "Arbitrum One",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    rpc_url: Some("https://arb1.arbitrum.io/rpc"),
};

/// OP Mainnet (Optimism).
pub const OPTIMISM: EvmChainInfo = EvmChainInfo {
    chain_id: "10",
    name: "OP Mainnet",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    rpc_url: Some("https://mainnet.optimism.io"),
};

/// Base.
pub const BASE: EvmChainInfo = EvmChainInfo {
    chain_id: "8453",
    name: "Base",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    rpc_url: Some("https://mainnet.base.org"),
};

/// Polygon PoS.
pub const POLYGON: EvmChainInfo = EvmChainInfo {
    chain_id: "137",
    name: "Polygon",
    spec_id: SpecId::CANCUN,
    native_symbol: "POL",
    rpc_url: Some("https://polygon-rpc.com"),
};

/// A generic local chain for fast tests (no real RPC).
pub const LOCAL: EvmChainInfo = EvmChainInfo {
    chain_id: "31337",
    name: "Local",
    spec_id: SpecId::CANCUN,
    native_symbol: "ETH",
    rpc_url: None,
};
