//! Commitment level requested from a (future) RPC endpoint.

/// Commitment level requested from a (future) RPC endpoint. Metadata only for the mock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commitment {
    /// Processed by the node, not yet voted on.
    Processed,
    /// Confirmed by a supermajority.
    Confirmed,
    /// Finalized (rooted).
    Finalized,
}
