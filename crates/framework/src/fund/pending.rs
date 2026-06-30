//! A funding requirement captured during setup, applied during `start()`.

use std::collections::HashMap;

use cross_vm_core::ChainKind;
use cross_vm_cosmwasm::{Addr, CwAsset};
use cross_vm_solana::{Address as SvmAddr, SvmAsset};
use cross_vm_solidity::{Address as EvmAddr, EvmAsset, U256};
use cross_vm_tron::{TronAddress, TronAsset};

use crate::any_chain::AnyChain;
use crate::error::EnvError;

/// A funding requirement captured during setup, applied during `start()`.
///
/// One variant per VM so the typed `who`/asset/amount are stored without erasure.
pub enum Pending {
    /// A CosmWasm funding requirement.
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
    pub(crate) async fn apply(
        self,
        chains: &mut HashMap<String, AnyChain>,
    ) -> Result<(), EnvError> {
        match self {
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
