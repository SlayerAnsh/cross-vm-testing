//! The uniform provider vocabulary implemented by every VM.

use crate::chain_spec::ChainSpec;
use crate::error::CrossVmError;

/// Chain-level simulation surface shared by every VM provider.
///
/// Associated types let each VM keep its own concrete `Address`/`Balance` while sharing
/// method names for accounts, native balances, and block progression. Contract and program
/// operations live on each VM crate's provider types (`store_code`, `deploy_create`,
/// `add_program`, and so on).
///
/// Methods are `async` so the same surface fits both the in-process mocks (whose bodies
/// are synchronous and simply do not `.await` anything) and the live RPC backends (which
/// await network I/O). `chain_info` stays synchronous since it only returns local metadata.
///
/// The returned futures are not guaranteed `Send`: the mock backends (`revm`, `litesvm`,
/// `cw-multi-test`) are not `Send`/`Sync`, so drive them on a current-thread runtime
/// (`#[tokio::test]` and `#[tokio::main]` default to this).
#[allow(async_fn_in_trait)]
pub trait ChainProvider: Sized {
    /// Concrete [`ChainSpec`] for this provider's chain.
    type Spec: ChainSpec;
    /// Account/address type (`Addr`, `Address`, `Pubkey`).
    type Address;
    /// Signing identity, when distinct from the address.
    type Account;
    /// Native-balance representation.
    type Balance;
    /// Provider error type. Must convert into [`CrossVmError`] for cross-vm scripts.
    type Error: Into<CrossVmError>;

    /// Metadata for the chain this provider targets.
    fn chain_info(&self) -> &Self::Spec;

    /// Create a fresh account. Mock providers also fund it with a default balance.
    async fn new_account(&mut self, label: &str) -> Self::Address;

    /// Read an account's native balance.
    async fn balance(&self, addr: &Self::Address) -> Result<Self::Balance, Self::Error>;

    /// Overwrite an account's native balance (mock-only convenience).
    async fn set_balance(
        &mut self,
        addr: &Self::Address,
        amount: Self::Balance,
    ) -> Result<(), Self::Error>;

    /// Current block height / slot.
    async fn block_height(&self) -> u64;

    /// Advance the chain by `n` blocks/slots.
    async fn advance_blocks(&mut self, n: u64);
}
