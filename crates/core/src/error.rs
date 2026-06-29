//! Unified error type shared across every VM.

use thiserror::Error;

use crate::chain_kind::ChainKind;

/// Unified error type so cross-vm scripts can use one `Result` across all VMs.
///
/// Each provider's own error converts into this via [`crate::ChainProvider::Error`]'s
/// `Into<CrossVmError>` bound.
#[derive(Debug, Error)]
pub enum CrossVmError {
    /// A feature is scaffolded but not yet implemented (e.g. live RPC in phase 1).
    #[error("{kind} provider: {what} is not implemented yet")]
    Unimplemented {
        /// VM the unimplemented feature belongs to.
        kind: ChainKind,
        /// Short description of the missing feature.
        what: String,
    },

    /// Deploying code failed.
    #[error("{kind} deploy failed: {reason}")]
    Deploy {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// Executing a call failed.
    #[error("{kind} execute failed: {reason}")]
    Execute {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// A query failed.
    #[error("{kind} query failed: {reason}")]
    Query {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// A balance read/write failed.
    #[error("{kind} balance op failed: {reason}")]
    Balance {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Underlying reason.
        reason: String,
    },

    /// Anything else, kept as a message.
    #[error("{kind} error: {reason}")]
    Other {
        /// VM where the failure occurred.
        kind: ChainKind,
        /// Message.
        reason: String,
    },

    /// A VM-specific accessor or operation was used on a chain of a different VM (for
    /// example calling `raw_evm()` on a CosmWasm response, or a `cw_*` hook on a Solana
    /// chain). The caller picked the wrong VM-specific path.
    #[error("wrong VM: expected {expected}, found {found}")]
    WrongVm {
        /// VM the caller expected.
        expected: ChainKind,
        /// VM actually present.
        found: ChainKind,
    },

    /// The backend genuinely does not provide the requested datum (distinct from
    /// [`CrossVmError::WrongVm`]): e.g. a transaction hash is not available on the
    /// in-process `cw-multi-test` backend even though the VM matches.
    #[error("{kind} backend does not support {what}")]
    Unsupported {
        /// VM whose backend lacks the datum.
        kind: ChainKind,
        /// What was requested.
        what: String,
    },

    /// A wallet operation failed: bad mnemonic, key derivation error, or `.env` load
    /// failure. Carries a free-form reason.
    #[error("wallet error: {reason}")]
    Wallet {
        /// Underlying reason.
        reason: String,
    },

    /// A wallet label was requested that is not present in the `WALLETS` const roster.
    #[error("unknown wallet label: {label}")]
    WalletNotFound {
        /// The label that was looked up.
        label: String,
    },

    /// A wallet's `EnvMnemonic`/`EnvPrivateKey` source referenced an environment variable
    /// that is not set in the process environment.
    #[error("secret env var `{var}` not set for wallet `{label}`")]
    SecretVarMissing {
        /// Wallet label whose source could not be resolved.
        label: String,
        /// The missing environment variable name.
        var: String,
    },
}

impl CrossVmError {
    /// Helper to build an [`CrossVmError::Unimplemented`].
    pub fn unimplemented(kind: ChainKind, what: impl Into<String>) -> Self {
        CrossVmError::Unimplemented {
            kind,
            what: what.into(),
        }
    }

    /// Helper to build a [`CrossVmError::WrongVm`].
    pub fn wrong_vm(expected: ChainKind, found: ChainKind) -> Self {
        CrossVmError::WrongVm { expected, found }
    }

    /// Helper to build a [`CrossVmError::Unsupported`].
    pub fn unsupported(kind: ChainKind, what: impl Into<String>) -> Self {
        CrossVmError::Unsupported {
            kind,
            what: what.into(),
        }
    }

    /// Helper to build a [`CrossVmError::Wallet`].
    pub fn wallet(reason: impl Into<String>) -> Self {
        CrossVmError::Wallet {
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_message_is_clear() {
        let e = CrossVmError::unimplemented(ChainKind::Evm, "live RPC execute");
        assert!(e.to_string().contains("not implemented"));
        assert!(e.to_string().contains("live RPC execute"));
    }
}
