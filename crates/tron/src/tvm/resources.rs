//! Energy/bandwidth accounting shim.
//!
//! A provider-layer approximation of Tron's two-resource model (energy for
//! computation, bandwidth for transaction bytes). This deliberately lives
//! OUTSIDE revm's gas loop: it is coarse account-level bookkeeping, NOT
//! per-opcode metering. Energy is granted by freezing TRX; bandwidth has a
//! small free daily allowance and otherwise burns TRX (the burn fallback is
//! not charged by this mock).
//!
//! Source: <https://developers.tron.network/docs/resource-model>

use std::collections::HashMap;

use crate::provider::address::TronAddress;

/// Free bandwidth points every account receives per day.
pub const FREE_BANDWIDTH_PER_DAY: u64 = 600;
/// Sun of frozen TRX that yields one unit of energy (approximation).
pub const SUN_PER_ENERGY: u64 = 100;
/// Sun burned per bandwidth point when the free allowance is exhausted.
pub const SUN_PER_BANDWIDTH: u64 = 1000;
/// Sun in one TRX.
pub const SUN_PER_TRX: u64 = 1_000_000;

/// Per-address resource state.
#[derive(Clone, Copy, Default)]
struct Account {
    /// Energy units currently available (gained by freezing TRX).
    energy: u64,
    /// Free bandwidth points already consumed today.
    bandwidth_consumed: u64,
}

/// Tracks per-address energy and bandwidth at the provider layer.
pub struct ResourceTracker {
    accounts: HashMap<TronAddress, Account>,
}

impl ResourceTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    /// Freeze `trx_sun` sun of TRX, granting `trx_sun / SUN_PER_ENERGY` energy
    /// units to `who` (added to any existing balance).
    pub fn freeze_for_energy(&mut self, who: TronAddress, trx_sun: u64) {
        let gained = trx_sun / SUN_PER_ENERGY;
        let acct = self.accounts.entry(who).or_default();
        acct.energy = acct.energy.saturating_add(gained);
    }

    /// Unfreeze `trx_sun` sun of TRX, removing `trx_sun / SUN_PER_ENERGY`
    /// energy units from `who` (saturating at zero).
    pub fn unfreeze(&mut self, who: TronAddress, trx_sun: u64) {
        let lost = trx_sun / SUN_PER_ENERGY;
        let acct = self.accounts.entry(who).or_default();
        acct.energy = acct.energy.saturating_sub(lost);
    }

    /// Current energy units for `who` (0 if the address is unknown).
    pub fn energy(&self, who: &TronAddress) -> u64 {
        self.accounts.get(who).map_or(0, |a| a.energy)
    }

    /// Remaining free bandwidth for `who` today.
    ///
    /// Note: daily reset is NOT modeled in v1, so consumption accumulates for
    /// the lifetime of the tracker.
    pub fn bandwidth(&self, who: &TronAddress) -> u64 {
        let consumed = self.accounts.get(who).map_or(0, |a| a.bandwidth_consumed);
        FREE_BANDWIDTH_PER_DAY.saturating_sub(consumed)
    }

    /// Deduct `tx_bytes` bandwidth points from `who`'s free daily allowance.
    ///
    /// Returns `true` if the transaction fit within the remaining free
    /// allowance (consumption recorded). Returns `false` if it would exceed the
    /// allowance, in which case `consumed` is left unchanged: this models the
    /// burn-for-fee fallback that the mock does not actually charge.
    ///
    /// Note: daily reset is NOT modeled in v1.
    pub fn consume_bandwidth(&mut self, who: &TronAddress, tx_bytes: usize) -> bool {
        let needed = tx_bytes as u64;
        let acct = self.accounts.entry(*who).or_default();
        let remaining = FREE_BANDWIDTH_PER_DAY.saturating_sub(acct.bandwidth_consumed);
        if needed > remaining {
            return false;
        }
        acct.bandwidth_consumed += needed;
        true
    }
}

impl Default for ResourceTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::address::address_from_label;

    #[test]
    fn freeze_grants_energy() {
        let mut rt = ResourceTracker::new();
        let alice = address_from_label("alice");
        rt.freeze_for_energy(alice, SUN_PER_TRX); // 1 TRX
        assert_eq!(rt.energy(&alice), 10_000);
    }

    #[test]
    fn unfreeze_reduces_energy() {
        let mut rt = ResourceTracker::new();
        let alice = address_from_label("alice");
        rt.freeze_for_energy(alice, 2 * SUN_PER_TRX);
        rt.unfreeze(alice, SUN_PER_TRX);
        assert_eq!(rt.energy(&alice), 10_000);
    }

    #[test]
    fn unfreeze_saturates_at_zero() {
        let mut rt = ResourceTracker::new();
        let bob = address_from_label("bob");
        rt.freeze_for_energy(bob, SUN_PER_TRX);
        rt.unfreeze(bob, 5 * SUN_PER_TRX);
        assert_eq!(rt.energy(&bob), 0);
    }

    #[test]
    fn energy_unknown_is_zero() {
        let rt = ResourceTracker::new();
        assert_eq!(rt.energy(&address_from_label("nobody")), 0);
    }

    #[test]
    fn bandwidth_free_then_exhausts() {
        let mut rt = ResourceTracker::new();
        let alice = address_from_label("alice");
        assert!(rt.consume_bandwidth(&alice, 300));
        assert!(rt.consume_bandwidth(&alice, 300));
        assert!(!rt.consume_bandwidth(&alice, 1));
        assert_eq!(rt.bandwidth(&alice), 0);
    }

    #[test]
    fn bandwidth_unknown_is_full() {
        let rt = ResourceTracker::new();
        assert_eq!(
            rt.bandwidth(&address_from_label("nobody")),
            FREE_BANDWIDTH_PER_DAY
        );
    }

    #[test]
    fn rejected_consume_leaves_balance_unchanged() {
        let mut rt = ResourceTracker::new();
        let alice = address_from_label("alice");
        assert!(!rt.consume_bandwidth(&alice, 601));
        assert_eq!(rt.bandwidth(&alice), FREE_BANDWIDTH_PER_DAY);
    }

    #[test]
    fn default_matches_new() {
        let rt = ResourceTracker::default();
        assert_eq!(rt.energy(&address_from_label("alice")), 0);
    }
}
