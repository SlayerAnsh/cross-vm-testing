//! A VM-agnostic account/address handle for cross-VM contract wrappers.
//!
//! A cross-VM test creates accounts via [`AnyChain::new_account`](crate::AnyChain) and
//! passes them uniformly to contract methods. Each per-VM hook recovers the native address
//! type with [`Account::cw`] / [`Account::evm`] / [`Account::svm`], which return a
//! [`CrossVmError::WrongVm`] if the account belongs to a different VM.
//!
//! The same type also stores a deployed **contract** address (a contract address is just an
//! address), which is what [`ContractBase`](crate::contract::ContractBase) holds.

use cross_vm_core::{ChainKind, CrossVmError};
use cross_vm_cosmwasm::Addr;
use cross_vm_solana::Address as SvmAddress;
use cross_vm_solidity::Address as EvmAddress;
use cross_vm_tron::TronAddress;

/// A VM-agnostic address: an account a test signs with, or a deployed contract address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Account {
    /// A CosmWasm bech32 address.
    CosmWasm(Addr),
    /// An EVM 20-byte address.
    Evm(EvmAddress),
    /// A Solana address (pubkey).
    Svm(SvmAddress),
    /// A Tron address.
    Tron(TronAddress),
}

impl Account {
    /// Which VM this address belongs to.
    pub fn kind(&self) -> ChainKind {
        match self {
            Account::CosmWasm(_) => ChainKind::CosmWasm,
            Account::Evm(_) => ChainKind::Evm,
            Account::Svm(_) => ChainKind::Svm,
            Account::Tron(_) => ChainKind::Tron,
        }
    }

    /// Recover the CosmWasm address, or [`CrossVmError::WrongVm`] if this is another VM.
    pub fn cw(&self) -> Result<&Addr, CrossVmError> {
        match self {
            Account::CosmWasm(a) => Ok(a),
            _ => Err(CrossVmError::wrong_vm(ChainKind::CosmWasm, self.kind())),
        }
    }

    /// Recover the EVM address, or [`CrossVmError::WrongVm`] if this is another VM.
    pub fn evm(&self) -> Result<&EvmAddress, CrossVmError> {
        match self {
            Account::Evm(a) => Ok(a),
            _ => Err(CrossVmError::wrong_vm(ChainKind::Evm, self.kind())),
        }
    }

    /// Recover the Solana address, or [`CrossVmError::WrongVm`] if this is another VM.
    pub fn svm(&self) -> Result<&SvmAddress, CrossVmError> {
        match self {
            Account::Svm(a) => Ok(a),
            _ => Err(CrossVmError::wrong_vm(ChainKind::Svm, self.kind())),
        }
    }

    /// Recover the Tron address, or [`CrossVmError::WrongVm`] if this is another VM.
    pub fn tron(&self) -> Result<&TronAddress, CrossVmError> {
        match self {
            Account::Tron(a) => Ok(a),
            _ => Err(CrossVmError::wrong_vm(ChainKind::Tron, self.kind())),
        }
    }
}

impl From<Addr> for Account {
    fn from(a: Addr) -> Self {
        Account::CosmWasm(a)
    }
}

impl From<EvmAddress> for Account {
    fn from(a: EvmAddress) -> Self {
        Account::Evm(a)
    }
}

impl From<SvmAddress> for Account {
    fn from(a: SvmAddress) -> Self {
        Account::Svm(a)
    }
}

impl From<TronAddress> for Account {
    fn from(a: TronAddress) -> Self {
        Account::Tron(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_vm_extractor_reports_expected_and_found() {
        let acct = Account::CosmWasm(Addr::unchecked("osmo1abc"));
        assert!(acct.cw().is_ok());
        let err = acct.evm().unwrap_err();
        assert!(matches!(
            err,
            CrossVmError::WrongVm {
                expected: ChainKind::Evm,
                found: ChainKind::CosmWasm,
            }
        ));
    }
}
