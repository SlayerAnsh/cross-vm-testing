//! A funding requirement captured during setup, applied during `start()`.

use std::collections::HashMap;

use cross_vm_core::ChainKind;
#[cfg(feature = "cw")]
use cross_vm_cosmwasm::{Addr, CwAsset};
#[cfg(feature = "solana")]
use cross_vm_solana::{Address as SvmAddr, SvmAsset};
#[cfg(feature = "evm")]
use cross_vm_solidity::{Address as EvmAddr, EvmAsset, U256};
#[cfg(feature = "tron")]
use cross_vm_tron::{TronAddress, TronAsset};

use crate::any_chain::AnyChain;
use crate::error::EnvError;

/// A funding requirement captured during setup, applied during `start()`.
///
/// One variant per VM so the typed `who`/asset/amount are stored without erasure.
pub enum Pending {
    /// A CosmWasm funding requirement.
    #[cfg(feature = "cw")]
    Cw {
        /// Chain label.
        label: String,
        /// Account.
        who: Addr,
        /// Asset.
        asset: CwAsset,
        /// Minimum amount.
        amount: u128,
    },
    /// An EVM funding requirement.
    #[cfg(feature = "evm")]
    Evm {
        /// Chain label.
        label: String,
        /// Account.
        who: EvmAddr,
        /// Asset.
        asset: EvmAsset,
        /// Minimum amount.
        amount: U256,
    },
    /// A Solana funding requirement.
    #[cfg(feature = "solana")]
    Svm {
        /// Chain label.
        label: String,
        /// Account.
        who: SvmAddr,
        /// Asset.
        asset: SvmAsset,
        /// Minimum amount.
        amount: u64,
    },
    /// A Tron funding requirement.
    #[cfg(feature = "tron")]
    Tron {
        /// Chain label.
        label: String,
        /// Account.
        who: TronAddress,
        /// Asset.
        asset: TronAsset,
        /// Minimum amount.
        amount: u64,
    },
}

impl Pending {
    /// Apply this requirement against the chain store.
    #[allow(unreachable_patterns)]
    pub(crate) async fn apply(
        self,
        chains: &mut HashMap<String, AnyChain>,
    ) -> Result<(), EnvError> {
        match self {
            #[cfg(feature = "cw")]
            Pending::Cw {
                label,
                who,
                asset,
                amount,
            } => {
                let who_str = who.to_string();
                match chains.get_mut(&label) {
                    Some(AnyChain::CosmWasm(c)) => c
                        .ensure_asset(&who, asset, amount)
                        .await
                        .map_err(|fe| EnvError::from_fund(label, who_str, fe)),
                    Some(other) => Err(wrong_vm(label, ChainKind::CosmWasm, other.kind())),
                    None => Err(EnvError::UnknownChain(label)),
                }
            }
            #[cfg(feature = "evm")]
            Pending::Evm {
                label,
                who,
                asset,
                amount,
            } => {
                let who_str = who.to_string();
                match chains.get_mut(&label) {
                    Some(AnyChain::Evm(c)) => c
                        .ensure_asset(&who, asset, amount)
                        .await
                        .map_err(|fe| EnvError::from_fund(label, who_str, fe)),
                    Some(other) => Err(wrong_vm(label, ChainKind::Evm, other.kind())),
                    None => Err(EnvError::UnknownChain(label)),
                }
            }
            #[cfg(feature = "solana")]
            Pending::Svm {
                label,
                who,
                asset,
                amount,
            } => {
                let who_str = who.to_string();
                match chains.get_mut(&label) {
                    Some(AnyChain::Svm(c)) => c
                        .ensure_asset(&who, asset, amount)
                        .await
                        .map_err(|fe| EnvError::from_fund(label, who_str, fe)),
                    Some(other) => Err(wrong_vm(label, ChainKind::Svm, other.kind())),
                    None => Err(EnvError::UnknownChain(label)),
                }
            }
            #[cfg(feature = "tron")]
            Pending::Tron {
                label,
                who,
                asset,
                amount,
            } => {
                let who_str = who.to_string();
                match chains.get_mut(&label) {
                    Some(AnyChain::Tron(c)) => c
                        .ensure_asset(&who, asset, amount)
                        .await
                        .map_err(|fe| EnvError::from_fund(label, who_str, fe)),
                    Some(other) => Err(wrong_vm(label, ChainKind::Tron, other.kind())),
                    None => Err(EnvError::UnknownChain(label)),
                }
            }
        }
    }
}

fn wrong_vm(label: String, expected: ChainKind, found: ChainKind) -> EnvError {
    EnvError::WrongVm {
        label,
        expected,
        found,
    }
}
