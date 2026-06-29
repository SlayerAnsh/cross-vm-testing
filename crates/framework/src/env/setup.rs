//! Setup-phase operations: injection, funding declarations, and `start()`.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

use cross_vm_core::WalletFactory;

use crate::any_chain::AnyChain;
use crate::env::multi_chain_env::MultiChainEnv;
use crate::env::phase::{Running, Setup};
use crate::error::EnvError;
use crate::fund::FundTarget;

impl MultiChainEnv<Setup> {
    /// Create a new environment in the setup phase with a shared wallet factory.
    pub fn new(label: impl Into<String>, wallets: Rc<WalletFactory>) -> Self {
        MultiChainEnv {
            label: label.into(),
            chains: HashMap::new(),
            pending: Vec::new(),
            wallets,
            _marker: PhantomData,
        }
    }

    /// Inject a chain under `label`. Accepts any mock or RPC provider, or a chain enum.
    pub fn inject(&mut self, label: impl Into<String>, chain: impl Into<AnyChain>) -> &mut Self {
        self.chains.insert(label.into(), chain.into());
        self
    }

    /// Declare that `who` must hold at least `amount` of the native coin `denom` on chain
    /// `label`.
    ///
    /// Testing funds native balances only, so `denom` is a raw string: the bank denom on
    /// CosmWasm (for example `"uosmo"`); EVM and Solana ignore it since each has a single
    /// native coin. The requirement is applied at [`MultiChainEnv::start`] (mock native funds
    /// are minted; RPC backends are validated). The label and VM are checked eagerly so an
    /// obvious mistake fails here rather than at `start`.
    pub fn fund<T: FundTarget>(
        &mut self,
        label: impl Into<String>,
        who: &T,
        denom: impl Into<String>,
        amount: T::Amount,
    ) -> Result<&mut Self, EnvError> {
        let label = label.into();
        match self.chains.get(&label) {
            None => return Err(EnvError::UnknownChain(label)),
            Some(c) if c.kind() != T::KIND => {
                return Err(EnvError::WrongVm {
                    label,
                    expected: T::KIND,
                    found: c.kind(),
                })
            }
            Some(_) => {}
        }
        self.pending
            .push(T::into_pending(label, who.clone(), denom.into(), amount));
        Ok(self)
    }

    /// Apply the funding plan and enter the running phase.
    ///
    /// Every declared requirement is applied; all failures are collected so the error
    /// reports every shortfall rather than just the first.
    pub async fn start(self) -> Result<MultiChainEnv<Running>, EnvError> {
        let MultiChainEnv {
            label,
            mut chains,
            pending,
            wallets,
            ..
        } = self;

        let mut errors = Vec::new();
        for op in pending {
            if let Err(e) = op.apply(&mut chains).await {
                errors.push(e);
            }
        }

        match errors.len() {
            0 => Ok(MultiChainEnv {
                label,
                chains,
                pending: Vec::new(),
                wallets,
                _marker: PhantomData,
            }),
            1 => Err(errors.pop().unwrap()),
            _ => Err(EnvError::Multiple(errors)),
        }
    }
}
