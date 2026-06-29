//! Uniform funding dispatch keyed on a VM's address type.
//!
//! [`FundTarget`] is implemented for each VM's address type. Because the three address
//! types are distinct, `MultiChainEnv::fund(label, &who, denom, amount)` resolves the VM
//! and amount type automatically from `who`'s type. Testing can fund native balances
//! only, so the asset is given as a raw denom string (the bank denom on CosmWasm;
//! informational on EVM/Solana, which each have a single native coin). Each call lowers
//! into a [`Pending`] requirement that [`crate::MultiChainEnv::start`] applies asynchronously.

use cross_vm_core::ChainKind;
use cross_vm_cosmwasm::{Addr, CwAsset};
use cross_vm_solana::{Address as SvmAddr, SvmAsset};
use cross_vm_solidity::{Address as EvmAddr, EvmAsset, U256};

use super::pending::Pending;

/// An address that can be funded inside a [`crate::MultiChainEnv`].
pub trait FundTarget: Clone + 'static {
    /// Amount type for this VM.
    type Amount: 'static;
    /// VM this target belongs to (used to validate the chain label up front).
    const KIND: ChainKind;

    /// Lower a `fund(...)` call into a native-funding [`Pending`] requirement.
    ///
    /// `denom` is the bank denom on CosmWasm; EVM and Solana ignore it because each has
    /// a single native coin.
    fn into_pending(label: String, who: Self, denom: String, amount: Self::Amount) -> Pending;
}

impl FundTarget for Addr {
    type Amount = u128;
    const KIND: ChainKind = ChainKind::CosmWasm;

    fn into_pending(label: String, who: Self, denom: String, amount: u128) -> Pending {
        Pending::Cw {
            label,
            who,
            asset: CwAsset::Native(denom),
            amount,
        }
    }
}

impl FundTarget for EvmAddr {
    type Amount = U256;
    const KIND: ChainKind = ChainKind::Evm;

    fn into_pending(label: String, who: Self, _denom: String, amount: U256) -> Pending {
        Pending::Evm {
            label,
            who,
            asset: EvmAsset::Native,
            amount,
        }
    }
}

impl FundTarget for SvmAddr {
    type Amount = u64;
    const KIND: ChainKind = ChainKind::Svm;

    fn into_pending(label: String, who: Self, _denom: String, amount: u64) -> Pending {
        Pending::Svm {
            label,
            who,
            asset: SvmAsset::Native,
            amount,
        }
    }
}
