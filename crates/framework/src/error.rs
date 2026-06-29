//! Errors raised by the cross-VM environment.

use cross_vm_core::{ChainKind, FundError};
use thiserror::Error;

use crate::shortfall::Shortfall;

/// Errors from building or driving a [`crate::MultiChainEnv`].
#[derive(Debug, Error)]
pub enum EnvError {
    /// No chain is registered under this label.
    #[error("unknown chain label: {0}")]
    UnknownChain(String),

    /// A label exists but belongs to a different VM than the operation expected.
    #[error("chain {label} is {found}, not {expected}")]
    WrongVm {
        /// The label queried.
        label: String,
        /// VM the caller expected.
        expected: ChainKind,
        /// VM actually stored.
        found: ChainKind,
    },

    /// A declared funding requirement was not met.
    #[error("funding shortfall: {0}")]
    Funding(Shortfall),

    /// Funding is unavailable on the targeted backend (for example live RPC).
    #[error("funding unimplemented: {0}")]
    Unimplemented(String),

    /// An underlying provider call failed.
    #[error("provider error: {0}")]
    Provider(String),

    /// Several errors occurred while applying the funding plan in `start()`.
    #[error("{} setup errors: {}", .0.len(), join(.0))]
    Multiple(Vec<EnvError>),
}

impl EnvError {
    /// Build from a VM [`FundError`], attaching chain/account context.
    pub(crate) fn from_fund(label: String, who: String, fe: FundError) -> Self {
        match fe {
            FundError::Shortfall {
                asset,
                required,
                actual,
            } => EnvError::Funding(Shortfall {
                label,
                who,
                asset,
                required,
                actual,
            }),
            FundError::Unimplemented(s) => EnvError::Unimplemented(s),
            FundError::Provider(s) => EnvError::Provider(s),
        }
    }
}

fn join(errs: &[EnvError]) -> String {
    errs.iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
