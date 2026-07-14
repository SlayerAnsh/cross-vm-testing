//! Metadata describing an EVM chain.

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
    /// Multiplier applied to an estimate to get the gas limit an
    /// [`EvmGasLimit::Estimated`](crate::EvmGasLimit::Estimated) op is submitted with. At least
    /// 1.0 (the config loader validates this); 1.3 by default, which covers the largest EIP-3529
    /// refund an estimate can hide (capped at a fifth of the gas burned, so a limit of 1.25x the
    /// billed figure always suffices).
    pub gas_adjustment: f64,
    /// Default public RPC endpoint, if known.
    pub rpc_url: Option<&'static str>,
}

impl EvmChainInfo {
    /// Numeric chain id used to configure the VM.
    pub fn numeric_id(&self) -> u64 {
        self.chain_id.parse().unwrap_or(1)
    }

    /// The limit to submit for an op that estimates at `estimated` gas: the estimate scaled by
    /// [`gas_adjustment`](Self::gas_adjustment), rounded up so the adjustment can never round a
    /// limit *below* the estimate it came from.
    pub fn adjusted_gas_limit(&self, estimated: u64) -> u64 {
        (estimated as f64 * self.gas_adjustment).ceil() as u64
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
