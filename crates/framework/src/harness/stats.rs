//! Opt-in per-operation diagnostics for a run.
//!
//! [`Stats`] answers "what did the fuzzer actually exercise?" — the failure mode where 80% of
//! generated swaps revert and the run tested almost nothing. It is **off by default**: a run only
//! collects it when the test opts in with [`Runner::with_stats`](crate::harness::Runner::with_stats),
//! so the zero-config path pays nothing.
//!
//! Operations are grouped by **variant name** (the leading token of their `Debug` rendering, e.g.
//! `Deposit { .. }` -> `"Deposit"`), so a bucket aggregates one op kind rather than one exact input.
//! This needs no naming method on [`Harness`](crate::harness::Harness): the derived `Debug` is
//! enough.

use std::collections::BTreeMap;
use std::time::Duration;

/// What `apply` did with one operation, as observed by the runner.
pub(crate) enum OpOutcome<'a> {
    /// The op was accepted (a legitimate success).
    Accepted,
    /// The op was rejected legitimately; the string is the revert reason.
    Rejected(&'a str),
    /// `apply` reported a confirmed SUT bug; the string is the detail.
    Bug(&'a str),
    /// A test-infrastructure failure; the string is the detail.
    Infra(&'a str),
}

/// Timing and outcome tally for one op label.
///
/// Serializes (behind the `serde` feature) via a hand-written `Serialize` impl into a
/// consumer-meaningful shape: `count`, the four outcome tallies, `errors`, and derived timing
/// (`total_ns`/`mean_ns`/`min_ns`/`max_ns`/`stddev_ns`), all as nanosecond `u64`s computed through
/// the same [`OpStat::total`]/[`OpStat::avg`]/[`OpStat::min`]/[`OpStat::max`]/[`OpStat::stddev`]
/// accessors a caller would use directly. The internal variance accumulator (`sum_sq_ns`) is a
/// pure computation artifact with no meaning to a report consumer, so it is never serialized
/// under its raw name; `stddev_ns` is what it exists to produce.
#[derive(Debug, Clone, Default)]
pub struct OpStat {
    /// Legitimate acceptances.
    pub accepted: usize,
    /// Legitimate rejections (reverts the model expected).
    pub rejected: usize,
    /// Confirmed SUT bugs surfaced by `apply`.
    pub bug: usize,
    /// Infrastructure failures during `apply`.
    pub infra: usize,
    /// Number of timed `apply` calls (equals the sum of the four outcome counts).
    pub count: usize,
    total_ns: u128,
    sum_sq_ns: u128,
    min_ns: u128,
    max_ns: u128,
    /// Error/revert reasons, counted (both legitimate rejections and bugs/infra land here).
    pub errors: BTreeMap<String, usize>,
}

impl OpStat {
    /// Fraction of applied ops that were rejected, in `0.0..=1.0`.
    pub fn reject_rate(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.rejected as f64 / self.count as f64
        }
    }

    /// Sum of all `apply` wall-clock times.
    pub fn total(&self) -> Duration {
        ns_to_duration(self.total_ns)
    }

    /// Minimum `apply` wall-clock, or zero if nothing was recorded.
    pub fn min(&self) -> Duration {
        if self.count == 0 {
            Duration::ZERO
        } else {
            ns_to_duration(self.min_ns)
        }
    }

    /// Maximum `apply` wall-clock.
    pub fn max(&self) -> Duration {
        ns_to_duration(self.max_ns)
    }

    /// Mean `apply` wall-clock.
    pub fn avg(&self) -> Duration {
        if self.count == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos((self.total_ns / self.count as u128) as u64)
        }
    }

    /// Population standard deviation of `apply` wall-clock (cheap, from running sums).
    pub fn stddev(&self) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let n = self.count as f64;
        let mean = self.total_ns as f64 / n;
        let var = (self.sum_sq_ns as f64 / n) - mean * mean;
        Duration::from_nanos(var.max(0.0).sqrt() as u64)
    }
}

/// Consumer-facing JSON shape for [`OpStat`]: outcome tallies plus derived nanosecond timing
/// (`total_ns`/`mean_ns`/`min_ns`/`max_ns`/`stddev_ns`), computed via the same accessors a Rust
/// caller would use. Deliberately hand-written rather than derived so the internal `sum_sq_ns`
/// variance accumulator never leaks into the wire format under its raw name.
#[cfg(feature = "serde")]
impl serde::Serialize for OpStat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("OpStat", 11)?;
        out.serialize_field("count", &self.count)?;
        out.serialize_field("accepted", &self.accepted)?;
        out.serialize_field("rejected", &self.rejected)?;
        out.serialize_field("bug", &self.bug)?;
        out.serialize_field("infra", &self.infra)?;
        out.serialize_field("total_ns", &(self.total().as_nanos() as u64))?;
        out.serialize_field("mean_ns", &(self.avg().as_nanos() as u64))?;
        out.serialize_field("min_ns", &(self.min().as_nanos() as u64))?;
        out.serialize_field("max_ns", &(self.max().as_nanos() as u64))?;
        out.serialize_field("stddev_ns", &(self.stddev().as_nanos() as u64))?;
        out.serialize_field("errors", &self.errors)?;
        out.end()
    }
}

/// Per-op-kind success/failure counts, `apply` timing, and an error breakdown, collected only when
/// a run opts in. Keyed by op variant name.
///
/// Serializes (behind `serde`) as `{"ops": {"Deposit": {...}, "Withdraw": {...}}}`: the single
/// private `ops` field derives like any other (visibility does not change what a derive emits),
/// and is left un-flattened (unlike [`crate::harness::Coverage`]'s `#[serde(transparent)]`) so a
/// `Stats` value nests as a `stats` key with an unambiguous shape inside `ErasedReport` rather
/// than colliding with a future top-level field of the same name.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Stats {
    ops: BTreeMap<String, OpStat>,
}

impl Stats {
    /// Record one `apply` call: its op `label`, wall-clock `elapsed`, and `outcome`.
    pub(crate) fn record(&mut self, label: &str, elapsed: Duration, outcome: OpOutcome<'_>) {
        let stat = self.ops.entry(label.to_string()).or_default();
        let ns = elapsed.as_nanos();
        if stat.count == 0 || ns < stat.min_ns {
            stat.min_ns = ns;
        }
        if ns > stat.max_ns {
            stat.max_ns = ns;
        }
        // Saturating: a pathological duration must degrade the derived stddev, not abort the run.
        stat.total_ns = stat.total_ns.saturating_add(ns);
        stat.sum_sq_ns = stat.sum_sq_ns.saturating_add(ns.saturating_mul(ns));
        stat.count += 1;
        match outcome {
            OpOutcome::Accepted => stat.accepted += 1,
            OpOutcome::Rejected(reason) => {
                stat.rejected += 1;
                *stat.errors.entry(reason.to_string()).or_default() += 1;
            }
            OpOutcome::Bug(detail) => {
                stat.bug += 1;
                *stat.errors.entry(detail.to_string()).or_default() += 1;
            }
            OpOutcome::Infra(detail) => {
                stat.infra += 1;
                *stat.errors.entry(detail.to_string()).or_default() += 1;
            }
        }
    }

    /// Per-label tallies in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &OpStat)> {
        self.ops.iter().map(|(n, s)| (n.as_str(), s))
    }

    /// The tally for one op label, if any was recorded.
    pub fn get(&self, label: &str) -> Option<&OpStat> {
        self.ops.get(label)
    }

    /// `true` if nothing was recorded (no ops applied).
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Emit a per-op summary block at `info`: counts, reject rate, timing, and top error reasons.
    pub fn log_summary(&self) {
        for (label, s) in self.iter() {
            tracing::info!(
                op = label,
                count = s.count,
                accepted = s.accepted,
                rejected = s.rejected,
                bug = s.bug,
                infra = s.infra,
                reject_rate = format!("{:.0}%", s.reject_rate() * 100.0),
                min_ms = s.min().as_secs_f64() * 1e3,
                avg_ms = s.avg().as_secs_f64() * 1e3,
                max_ms = s.max().as_secs_f64() * 1e3,
                stddev_ms = s.stddev().as_secs_f64() * 1e3,
                "op stats"
            );
            // Surface the loudest few error/revert reasons for this op.
            let mut reasons: Vec<(&String, &usize)> = s.errors.iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(a.1));
            for (reason, n) in reasons.into_iter().take(3) {
                tracing::info!(op = label, count = n, reason = %reason, "op error reason");
            }
        }
    }
}

/// Saturating nanosecond -> [`Duration`] conversion (`u128` accumulators, `u64` constructor).
fn ns_to_duration(ns: u128) -> Duration {
    Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
}

/// The op label used to group [`Stats`]: the leading identifier token of the op's `Debug` rendering
/// (`Deposit { chain, .. }` -> `"Deposit"`). Groups by variant, not by exact input. A `Debug`
/// rendering that starts with a non-identifier character has no variant token to take; the label
/// falls back to the rendering truncated to a few leading characters, keeping bucket keys bounded.
pub fn op_label<Op: core::fmt::Debug>(op: &Op) -> String {
    const FALLBACK_LEN: usize = 32;
    let dbg = format!("{op:?}");
    let end = dbg
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(dbg.len());
    if end == 0 {
        dbg.chars().take(FALLBACK_LEN).collect()
    } else {
        dbg[..end].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Op {
        Deposit { amount: u128 },
    }

    struct WeirdDebug;
    impl core::fmt::Debug for WeirdDebug {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "<op with a very long non-identifier debug rendering>")
        }
    }

    #[test]
    fn op_label_takes_the_leading_variant_token() {
        assert_eq!(op_label(&Op::Deposit { amount: 5 }), "Deposit");
    }

    #[test]
    fn op_label_truncates_non_identifier_debug_renderings() {
        let label = op_label(&WeirdDebug);
        assert_eq!(label.chars().count(), 32, "bounded fallback label");
        assert!(label.starts_with("<op with"));
    }

    #[test]
    fn timing_accumulators_survive_u64_nanosecond_overflow() {
        let mut stats = Stats::default();
        // ~584 years: past u64::MAX nanoseconds; min/max must not truncate on record.
        let huge = Duration::from_secs(u64::MAX / 1_000_000_000 + 1);
        stats.record("Op", huge, OpOutcome::Accepted);
        let s = stats.get("Op").unwrap();
        // Saturates at the Duration accessor, not silently wrapped at record time.
        assert_eq!(s.max(), Duration::from_nanos(u64::MAX));
        assert_eq!(s.min(), Duration::from_nanos(u64::MAX));
    }

    // -------------------------------------------------------------------------------------
    // serde (spec section 9): OpStat/Stats shapes. Gated on `cli` (not just `serde`) for the
    // same reason as `outcome.rs`'s equivalent module: `serde_json`, used here to assert JSON
    // shape, is only pulled in by `cli`.
    // -------------------------------------------------------------------------------------

    #[cfg(feature = "cli")]
    mod serde_shapes {
        use super::*;

        #[test]
        fn op_stat_serializes_outcome_counts_and_timing_fields() {
            let mut stats = Stats::default();
            stats.record("Deposit", Duration::from_millis(5), OpOutcome::Accepted);
            stats.record(
                "Deposit",
                Duration::from_millis(15),
                OpOutcome::Rejected("cap"),
            );

            let stat = stats.get("Deposit").unwrap();
            let value = serde_json::to_value(stat).unwrap();

            assert_eq!(value["count"], 2);
            assert_eq!(value["accepted"], 1);
            assert_eq!(value["rejected"], 1);
            assert_eq!(value["bug"], 0);
            assert_eq!(value["infra"], 0);
            assert_eq!(value["errors"], serde_json::json!({"cap": 1}));
            // Derived, consumer-meaningful timing fields (nanoseconds), computed via the same
            // accessors a Rust caller would use — not the raw internal accumulators.
            assert_eq!(value["total_ns"], 20_000_000u64);
            assert_eq!(value["mean_ns"], 10_000_000u64);
            assert_eq!(value["min_ns"], 5_000_000u64);
            assert_eq!(value["max_ns"], 15_000_000u64);
            assert_eq!(value["stddev_ns"], stat.stddev().as_nanos() as u64);
            // The raw variance accumulator must never leak into the wire format.
            assert!(value.get("sum_sq_ns").is_none());
            // Exactly the intended field set, nothing extra.
            let obj = value.as_object().unwrap();
            let expected: std::collections::BTreeSet<&str> = [
                "count",
                "accepted",
                "rejected",
                "bug",
                "infra",
                "total_ns",
                "mean_ns",
                "min_ns",
                "max_ns",
                "stddev_ns",
                "errors",
            ]
            .into_iter()
            .collect();
            let actual: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
            assert_eq!(actual, expected);
        }

        #[test]
        fn stats_serializes_as_an_ops_object_keyed_by_op_label() {
            let mut stats = Stats::default();
            stats.record("Deposit", Duration::from_millis(1), OpOutcome::Accepted);
            stats.record("Withdraw", Duration::from_millis(1), OpOutcome::Accepted);

            let value = serde_json::to_value(&stats).unwrap();
            let ops = value["ops"].as_object().expect("ops is an object");
            assert!(ops.contains_key("Deposit"));
            assert!(ops.contains_key("Withdraw"));
        }
    }
}
