//! Heterogeneous storage for chains of different VMs.

use cross_vm_core::{BlockTime, ChainKind, ChainProvider, ChainSpec, CrossVmError, WalletLabel};
#[cfg(feature = "cw")]
use cross_vm_cosmwasm::{CwChain, CwGasLimit, CwMockProvider, CwRpcProvider};
#[cfg(feature = "solana")]
use cross_vm_solana::{SvmChain, SvmComputeBudget, SvmMockProvider, SvmRpcProvider};
#[cfg(feature = "evm")]
use cross_vm_solidity::{EvmChain, EvmGasLimit, EvmMockProvider, EvmRpcProvider, U256};
#[cfg(feature = "tron")]
use cross_vm_tron::{TronChain, TronMockProvider, TronRpcProvider};

use crate::contract::Account;

/// A chain of any supported VM, stored by the environment.
///
/// `ChainProvider` is not object safe, so this enum (rather than a trait object) is how
/// CosmWasm, EVM, and Solana chains live side by side in one map.
// Per-VM mock states differ in size; the gap is inherent to wrapping three VMs.
#[derive(Clone)]
pub enum AnyChain {
    /// A CosmWasm chain.
    #[cfg(feature = "cw")]
    CosmWasm(CwChain),
    /// An EVM chain.
    #[cfg(feature = "evm")]
    Evm(EvmChain),
    /// A Solana chain.
    #[cfg(feature = "solana")]
    Svm(SvmChain),
    /// A Tron chain.
    #[cfg(feature = "tron")]
    Tron(TronChain),
}

impl AnyChain {
    /// Which VM this chain belongs to.
    pub fn kind(&self) -> ChainKind {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(_) => ChainKind::CosmWasm,
            #[cfg(feature = "evm")]
            AnyChain::Evm(_) => ChainKind::Evm,
            #[cfg(feature = "solana")]
            AnyChain::Svm(_) => ChainKind::Svm,
            #[cfg(feature = "tron")]
            AnyChain::Tron(_) => ChainKind::Tron,
        }
    }

    /// Create a fresh account on this chain and return it as a VM-agnostic [`Account`].
    ///
    /// On the mock backends the account is also funded with a default native balance, so a
    /// cross-VM test can deploy and execute without an explicit funding step.
    pub async fn new_account(&mut self, label: &str) -> Account {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(c) => Account::CosmWasm(c.new_account(label).await),
            #[cfg(feature = "evm")]
            AnyChain::Evm(c) => Account::Evm(c.new_account(label).await),
            #[cfg(feature = "solana")]
            AnyChain::Svm(c) => Account::Svm(c.new_account(label).await),
            #[cfg(feature = "tron")]
            AnyChain::Tron(c) => Account::Tron(c.new_account(label).await),
        }
    }

    /// Current block height / slot of the underlying chain.
    ///
    /// Forwards to the VM provider's [`ChainProvider::block_height`]. Used by the endurance
    /// runner to confirm block progression across a multi-chain world.
    pub async fn block_height(&self) -> u64 {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(c) => c.block_height().await,
            #[cfg(feature = "evm")]
            AnyChain::Evm(c) => c.block_height().await,
            #[cfg(feature = "solana")]
            AnyChain::Svm(c) => c.block_height().await,
            #[cfg(feature = "tron")]
            AnyChain::Tron(c) => c.block_height().await,
        }
    }

    /// Chain identifier from the underlying chain's spec (no typecast needed).
    ///
    /// Forwards to the VM provider's [`ChainProvider::chain_info`] and reads the spec's
    /// [`ChainSpec::chain_id`], so callers get the id without matching on the VM variant.
    pub fn chain_id(&self) -> &str {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(c) => c.chain_info().chain_id(),
            #[cfg(feature = "evm")]
            AnyChain::Evm(c) => c.chain_info().chain_id(),
            #[cfg(feature = "solana")]
            AnyChain::Svm(c) => c.chain_info().chain_id(),
            #[cfg(feature = "tron")]
            AnyChain::Tron(c) => c.chain_info().chain_id(),
        }
    }

    /// Advance the underlying chain by `n` blocks/slots.
    ///
    /// Forwards to the VM provider's [`ChainProvider::advance_blocks`]. The harness `advance`
    /// hook calls this on every chain it holds so time progresses uniformly.
    pub async fn advance_blocks(&mut self, n: u64, time: BlockTime) {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(c) => c.advance_blocks(n, time).await,
            #[cfg(feature = "evm")]
            AnyChain::Evm(c) => c.advance_blocks(n, time).await,
            #[cfg(feature = "solana")]
            AnyChain::Svm(c) => c.advance_blocks(n, time).await,
            #[cfg(feature = "tron")]
            AnyChain::Tron(c) => c.advance_blocks(n, time).await,
        }
    }

    /// Send `amount` of native `denom` from the `wallet` signer to `to`, and return the
    /// transaction hash.
    ///
    /// Forwards to the VM's own `transfer_funds`, so a test can move native funds without
    /// matching on the VM variant. `to` is recovered with the [`Account`] accessor for this
    /// chain's VM: a recipient from another VM is a [`CrossVmError::WrongVm`].
    ///
    /// `amount` is in the chain's **base units**, never whole tokens: wei on EVM, lamports on
    /// Solana, sun on Tron, and the bank denom's own units on CosmWasm (`uosmo`, not `OSMO`).
    /// `u128` is the widest of the per-VM amount types; Solana and Tron take `u64`, so an
    /// amount that does not fit is a [`CrossVmError::Balance`] naming the base unit rather
    /// than a silent truncation.
    ///
    /// `denom` is passed through verbatim; each VM validates it below this layer. On Solana
    /// only the mock backend transfers, the RPC backend returns
    /// [`CrossVmError::Unimplemented`].
    ///
    /// There is no limit parameter, deliberately: the VM limits it would forward to are not the
    /// same quantity (`Exact(n)` is gas on EVM, sun on Tron, compute units on Solana), which is
    /// exactly why the limit types are per-VM. `Estimated` is the only limit that means the same
    /// thing on all four, so this layer always resolves it (Tron's `transfer_funds` takes no
    /// limit at all: a `TransferContract` runs no code and burns only bandwidth). A caller
    /// needing an exact limit downcasts to the concrete chain, the same escape hatch contract
    /// ops already use.
    pub async fn transfer_funds(
        &self,
        to: &Account,
        denom: &str,
        amount: u128,
        wallet: WalletLabel<'_>,
    ) -> Result<String, CrossVmError> {
        match self {
            #[cfg(feature = "cw")]
            AnyChain::CosmWasm(c) => Ok(c
                .transfer_funds(to.cw()?, denom, amount, wallet, CwGasLimit::Estimated)
                .await?),
            #[cfg(feature = "evm")]
            AnyChain::Evm(c) => Ok(c
                .transfer_funds(
                    to.evm()?,
                    denom,
                    U256::from(amount),
                    wallet,
                    EvmGasLimit::Estimated,
                )
                .await?),
            #[cfg(feature = "solana")]
            AnyChain::Svm(c) => {
                let lamports = base_units_u64(amount, ChainKind::Svm, "lamports")?;
                Ok(c.transfer_funds(
                    to.svm()?,
                    denom,
                    lamports,
                    wallet,
                    SvmComputeBudget::Estimated,
                )
                .await?)
            }
            #[cfg(feature = "tron")]
            AnyChain::Tron(c) => {
                let sun = base_units_u64(amount, ChainKind::Tron, "sun")?;
                Ok(c.transfer_funds(to.tron()?, denom, sun, wallet).await?)
            }
        }
    }
}

/// Narrow a base-unit `amount` to the `u64` that Solana and Tron carry balances in.
///
/// Overflow is an error, not a truncation: a caller who asks to move more than `u64::MAX`
/// base units is asking for something the chain cannot represent.
#[cfg(any(feature = "solana", feature = "tron"))]
fn base_units_u64(amount: u128, kind: ChainKind, unit: &str) -> Result<u64, CrossVmError> {
    u64::try_from(amount).map_err(|_| CrossVmError::Balance {
        kind,
        reason: format!("amount {amount} exceeds the u64 range of {kind} {unit}"),
    })
}

macro_rules! into_any {
    ($($ty:ty => $variant:ident via $wrap:ident),* $(,)?) => {
        $(
            impl From<$ty> for AnyChain {
                fn from(p: $ty) -> Self {
                    AnyChain::$variant($wrap::from(p))
                }
            }
        )*
    };
}

#[cfg(feature = "cw")]
into_any! {
    CwMockProvider  => CosmWasm via CwChain,
    CwRpcProvider   => CosmWasm via CwChain,
}
#[cfg(feature = "evm")]
into_any! {
    EvmMockProvider => Evm      via EvmChain,
    EvmRpcProvider  => Evm      via EvmChain,
}
#[cfg(feature = "solana")]
into_any! {
    SvmMockProvider => Svm      via SvmChain,
    SvmRpcProvider  => Svm      via SvmChain,
}
#[cfg(feature = "tron")]
into_any! {
    TronMockProvider => Tron via TronChain,
    TronRpcProvider  => Tron via TronChain,
}

#[cfg(feature = "cw")]
impl From<CwChain> for AnyChain {
    fn from(c: CwChain) -> Self {
        AnyChain::CosmWasm(c)
    }
}
#[cfg(feature = "evm")]
impl From<EvmChain> for AnyChain {
    fn from(c: EvmChain) -> Self {
        AnyChain::Evm(c)
    }
}
#[cfg(feature = "solana")]
impl From<SvmChain> for AnyChain {
    fn from(c: SvmChain) -> Self {
        AnyChain::Svm(c)
    }
}
#[cfg(feature = "tron")]
impl From<TronChain> for AnyChain {
    fn from(c: TronChain) -> Self {
        AnyChain::Tron(c)
    }
}
