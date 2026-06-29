//! A fundable asset on a Solana chain.

use solana_address::Address;

/// A fundable asset on a Solana chain.
#[derive(Debug, Clone)]
pub enum SvmAsset {
    /// Native SOL (lamports).
    Native,
    /// An SPL token, identified by the holder's token account to read.
    Spl(Address),
}
