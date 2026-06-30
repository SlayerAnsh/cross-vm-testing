//! Shared mock clock for the in-process providers.
//!
//! Every mock VM (CosmWasm, EVM, Solana) stamps its blocks with the SAME fixed timestamp so that a
//! cross-VM packet whose timeout is computed on one chain and checked on another compares correctly.
//! Without a shared clock the chains drift (e.g. revm starts near epoch 0 while cw-multi-test uses a
//! 2019-era default), and any timeout a sender stamps looks already-expired to the receiver.

use std::time::{SystemTime, UNIX_EPOCH};

/// The fixed block timestamp (unix seconds) every mock provider starts at. A round, near-future
/// value (2026-01-01T00:00:00Z) well clear of cw-multi-test's 2019-era default.
pub const MOCK_BLOCK_TIMESTAMP: u64 = 1_767_225_600;

/// How a block-advance sets the resulting block timestamp (unix seconds). Block height/slot count
/// is controlled separately by the advance's `n` argument; this only governs the clock.
#[derive(Clone, Copy, Debug)]
pub enum BlockTime {
    /// Set the timestamp to this exact unix-seconds value.
    Custom(u64),
    /// Set the timestamp to the host's current wall-clock time.
    Now,
    /// Add this many seconds to the current block timestamp.
    Increment(u64),
}

impl BlockTime {
    /// Resolve to the new absolute unix-seconds timestamp given the chain's current one.
    pub fn apply(self, current: u64) -> u64 {
        match self {
            BlockTime::Custom(ts) => ts,
            BlockTime::Now => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(current),
            BlockTime::Increment(secs) => current.saturating_add(secs),
        }
    }
}
