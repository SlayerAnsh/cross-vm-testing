//! [`ErasedReport`]/[`ErasedFailure`]: the mode-agnostic outcome the registry hands back to the
//! CLI, plus [`erase_report`], the conversion from a monomorphized [`RunReport`].
//!
//! The registry's `run` closure (`crate::registry`) is generic over the registered
//! [`Harness`](harness_core::Harness), so no `dyn Harness` ever exists inside it; `ErasedReport`
//! is the one place a run's outcome crosses from "generic over `H::Operation`" into
//! "harness-agnostic data the CLI can print or serialize as JSON" (spec section 7).

use harness_core::{Coverage, FailureKind, RunReport, Stats};

/// A boxed, pinned, `!Send` future. Every erased future in the registry uses this alias, never
/// the `futures` crate: the stack is single-threaded by construction (`Rc<WalletFactory>`,
/// `#[tokio::main(flavor = "current_thread")]`; spec section 3), so nothing here carries a `Send`
/// bound.
pub(crate) type LocalBoxFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// Mode-agnostic outcome of one profile run: [`RunReport`] with the operation type erased.
///
/// Produced by `erase_report` from a monomorphized `RunReport<H::Operation>`. This is the
/// shape the CLI prints, maps to an exit code, and serializes as one entry of the
/// `--json-report` payload's `profiles` array (spec section 9); `report::JsonReport`
/// wraps a `&[ErasedReport]` in the envelope written once per invocation. `elapsed` serializes
/// with `Duration`'s default serde representation (`{"secs": .., "nanos": ..}`), matching how
/// every other `Duration` field in this crate's serde surface serializes; no custom
/// millisecond `serialize_with` was added, since a single, predictable shape across the report
/// is worth more than shaving one nesting level.
#[derive(Debug, serde::Serialize)]
pub struct ErasedReport {
    /// The registered harness name this run used.
    pub harness: String,
    /// The profile name this run used.
    pub profile: String,
    /// The mode label (`"fuzz"`, `"invariant"` today; `"endurance"` / `"case"` / `"replay"` once
    /// later tasks fill in those drivers).
    pub mode: String,
    /// The base seed the run was driven with (the per-case seed for a failing fuzz case, the
    /// profile's own seed for invariant).
    pub seed: u64,
    /// Total operations applied.
    pub steps: usize,
    /// How many invariant checks were skipped (precondition not yet met) over the run.
    pub skipped: usize,
    /// Per-invariant tallies (held / skipped / violated), keyed by the invariant's `Debug` name.
    pub coverage: Coverage,
    /// Collected per-op diagnostics, present only when the profile enabled
    /// [`stats`](crate::ResolvedProfile::stats).
    pub stats: Option<Stats>,
    /// Wall-clock time the whole profile run took: every fuzz case's setup and drive combined,
    /// or the one setup and drive for invariant.
    pub elapsed: std::time::Duration,
    /// The first failure encountered, if any. `None` means the run passed.
    pub failure: Option<ErasedFailure>,
}

/// The type-erased counterpart of [`harness_core::Failure`].
#[derive(Debug, serde::Serialize)]
pub struct ErasedFailure {
    /// 1-based index of the operation that failed, or `0` for a pre-operation failure.
    pub step: usize,
    /// What went wrong.
    pub kind: FailureKind,
    /// `Debug` rendering of the failing op, for the human log. `None` for a pre-operation
    /// failure (no op was in flight).
    pub op_debug: Option<String>,
    /// The full operation history up to and including the failing op, serialized as JSON; feeds
    /// the replay artifact writer. The shrunk sequence when `erase_report`'s `shrunk` argument
    /// is `true`, the raw history otherwise.
    pub history: serde_json::Value,
    /// Whether `history` above is the auto-shrunk sequence (`resolved.shrink` was `true` and the
    /// run failed) or the raw, unshrunk history. Set by `erase_report`'s caller
    /// (`crate::registry`), never derived here.
    pub shrunk: bool,
}

/// Converts a monomorphized `RunReport<H::Operation>` into a harness-agnostic [`ErasedReport`].
///
/// Copies `seed`/`steps`/`skipped`/`coverage` verbatim and maps `failure` into an
/// [`ErasedFailure`]: `op_debug` is the `Debug` rendering of the failing op (if any), and
/// `history` is the full op history encoded through the harness's own [`ConfigOps::encode_op`]
/// codec (externally tagged single-key objects), so the replay artifact it feeds round-trips
/// back through the same decoder. Encoding cannot fail, so this function is infallible.
///
/// `shrunk` is the caller's own determination (`crate::registry`'s `maybe_shrink`): `true`
/// when `report.failure.history` is already the auto-shrunk sequence, `false` when it is the raw
/// history (shrink disabled, or this profile's mode never shrinks). This function does not shrink
/// anything itself; it only stamps the flag onto the erased failure.
#[allow(clippy::too_many_arguments)]
pub(crate) fn erase_report<H: harness_core::ConfigOps>(
    codec: &H,
    report: RunReport<H::Operation>,
    harness: String,
    profile: String,
    mode: String,
    stats: Option<Stats>,
    elapsed: std::time::Duration,
    shrunk: bool,
) -> ErasedReport {
    let failure = report.failure.map(|f| ErasedFailure {
        step: f.step,
        op_debug: f.op.as_ref().map(|o| format!("{o:?}")),
        history: serde_json::Value::Array(f.history.iter().map(|op| codec.encode_op(op)).collect()),
        kind: f.kind,
        shrunk,
    });

    ErasedReport {
        harness,
        profile,
        mode,
        seed: report.seed,
        steps: report.steps,
        skipped: report.skipped,
        coverage: report.coverage,
        stats,
        elapsed,
        failure,
    }
}
