//! Outcomes of applying an operation and of a whole run.
//!
//! Three things must stay distinct:
//! - A legitimate **acceptance** vs a legitimate **rejection** ([`Verdict`]). A withdraw of
//!   more than the balance *should* revert; that is not a bug.
//! - A discovered **bug** vs a test-**infrastructure** failure ([`HarnessError`]). An over-
//!   withdraw that the contract *accepted* is a bug; a failed deploy is infrastructure.
//! - An **invariant** violation ([`Violation`]), surfaced by [`crate::harness::Harness::check`].
//!
//! The classification policy lives inside the developer's `apply`, the only place that knows an
//! operation's semantics. [`classify`] collapses the common four-way match into one call.

use cross_vm_core::CrossVmError;

use crate::contract::AppResponse;
use crate::error::EnvError;

/// How the system-under-test responded to an operation the developer judged legitimate to attempt.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// The operation succeeded and the model expected it to.
    Accepted,
    /// The operation was rejected, and that rejection is expected (e.g. withdraw > balance).
    Rejected {
        /// The revert reason, for diagnostics.
        reason: String,
    },
}

/// An error that ends a run as a failure (as opposed to a legitimate [`Verdict::Rejected`]).
#[derive(Debug)]
pub enum HarnessError {
    /// A confirmed bug in the system-under-test: an operation the model said must succeed was
    /// rejected, or one that must fail was accepted.
    Bug(String),
    /// A test-infrastructure failure (deploy/RPC/model desync) — usually a harness bug, not a
    /// SUT bug, so it is reported separately.
    Infra(CrossVmError),
}

impl core::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HarnessError::Bug(m) => write!(f, "bug: {m}"),
            HarnessError::Infra(e) => write!(f, "infra: {e}"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<CrossVmError> for HarnessError {
    fn from(e: CrossVmError) -> Self {
        HarnessError::Infra(e)
    }
}

impl From<EnvError> for HarnessError {
    /// An environment build/drive failure (inject/fund/start, or an unknown-chain lookup) is
    /// test infrastructure, not a SUT bug, so it maps to [`HarnessError::Infra`].
    fn from(e: EnvError) -> Self {
        HarnessError::Infra(CrossVmError::wallet(e.to_string()))
    }
}

/// A broken invariant, returned by [`crate::harness::Harness::check`].
#[derive(Debug, Clone)]
pub struct Violation {
    /// Human-readable detail of how the invariant was broken.
    pub detail: String,
}

impl Violation {
    /// Build a violation from any displayable detail.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// The result of checking one invariant against the current state.
///
/// Distinct from a plain `Result` so an invariant can declare itself **not yet applicable**
/// (its precondition has not occurred yet, e.g. "no counter has been incremented") instead of
/// being forced to vacuously pass. A [`Skipped`](CheckOutcome::Skipped) check carries a reason
/// for the report and never fails the run.
#[derive(Debug, Clone)]
pub enum CheckOutcome {
    /// The invariant was applicable and held.
    Held,
    /// The invariant is not applicable yet; the string is a human-readable reason.
    Skipped(String),
    /// The invariant was applicable and broke.
    Violated(Violation),
}

impl CheckOutcome {
    /// Convenience: a violation from any displayable detail.
    pub fn violated(detail: impl Into<String>) -> Self {
        CheckOutcome::Violated(Violation::new(detail))
    }

    /// Convenience: a skip from any displayable reason.
    pub fn skipped(reason: impl Into<String>) -> Self {
        CheckOutcome::Skipped(reason.into())
    }
}

impl From<Result<(), Violation>> for CheckOutcome {
    /// Lift the old `Result<(), Violation>` shape: `Ok` -> `Held`, `Err` -> `Violated`.
    fn from(r: Result<(), Violation>) -> Self {
        match r {
            Ok(()) => CheckOutcome::Held,
            Err(v) => CheckOutcome::Violated(v),
        }
    }
}

/// Why a run failed.
#[derive(Debug, Clone)]
pub enum FailureKind {
    /// `apply` reported a confirmed SUT bug.
    Bug(String),
    /// An invariant was violated after an operation.
    Invariant {
        /// The `Debug` rendering of the violated invariant.
        name: String,
        /// Detail from the [`Violation`].
        detail: String,
    },
    /// A test-infrastructure failure (e.g. `state()` or `advance()` failed).
    Infra(String),
}

/// A single failing step, with everything needed to replay it deterministically.
#[derive(Debug, Clone)]
pub struct Failure<Op> {
    /// 1-based index of the operation that failed, or `0` for a pre-operation failure such as
    /// `state()` itself failing.
    pub step: usize,
    /// The operation that triggered the failure. `None` for a pre-operation failure.
    pub op: Option<Op>,
    /// The full operation history up to and including the failing op, for [`replay`].
    ///
    /// [`replay`]: crate::harness::Runner::replay
    pub history: Vec<Op>,
    /// What went wrong.
    pub kind: FailureKind,
}

/// The result of a run: the seed and mode that produced it, the number of steps taken, and the
/// first failure if any. `failure.is_none()` means the run passed.
#[derive(Debug, Clone)]
pub struct RunReport<Op> {
    /// The base seed the run was driven with.
    pub seed: u64,
    /// The mode label (`"fuzz"`, `"invariant"`, `"endurance"`, `"case"`, `"replay"`).
    pub mode: &'static str,
    /// Total operations applied.
    pub steps: usize,
    /// How many invariant checks were skipped (precondition not yet met) over the run.
    pub skipped: usize,
    /// The first failure encountered, if any.
    pub failure: Option<Failure<Op>>,
}

impl<Op> RunReport<Op> {
    /// `true` if the run encountered no failure.
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Collapse the four-way (expected, result) match into one call.
///
/// `expected_ok` is the model's prediction for whether this operation should be accepted.
/// `on_ok` mutates the model after a confirmed acceptance. Only [`CrossVmError::Execute`]
/// counts as a legitimate revert; any other error is treated as infrastructure.
///
/// ```ignore
/// let ok = world.model.can_withdraw(user, amount);
/// let res = world.vault.withdraw(who, amount).await;
/// classify(ok, res, || world.model.apply_withdraw(user, amount),
///          "over-withdraw was accepted", "valid withdraw reverted")
/// ```
pub fn classify<T>(
    expected_ok: bool,
    res: Result<AppResponse<T>, CrossVmError>,
    on_ok: impl FnOnce(),
    bug_if_accepted: &str,
    bug_if_reverted: &str,
) -> Result<Verdict, HarnessError> {
    match res {
        Ok(_) if expected_ok => {
            on_ok();
            Ok(Verdict::Accepted)
        }
        // Succeeded when the model said it must fail: a bug.
        Ok(_) => Err(HarnessError::Bug(bug_if_accepted.into())),
        // Only a revert (`Execute`) is a legitimate rejection.
        Err(CrossVmError::Execute { reason, .. }) if !expected_ok => {
            Ok(Verdict::Rejected { reason })
        }
        // Reverted when the model said it must succeed: a bug.
        Err(CrossVmError::Execute { reason, .. }) => {
            Err(HarnessError::Bug(format!("{bug_if_reverted}: {reason}")))
        }
        // Any non-revert error is infrastructure, regardless of the model's prediction.
        Err(e) => Err(HarnessError::Infra(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cross_vm_core::ChainKind;

    fn revert() -> CrossVmError {
        CrossVmError::Execute {
            kind: ChainKind::Evm,
            reason: "insufficient balance".into(),
        }
    }

    #[test]
    fn accepted_runs_on_ok() {
        let mut applied = false;
        let v = classify::<()>(
            true,
            Ok(AppResponse::evm((), Default::default(), vec![])),
            || applied = true,
            "a",
            "b",
        )
        .unwrap();
        assert!(matches!(v, Verdict::Accepted));
        assert!(applied);
    }

    #[test]
    fn expected_revert_is_rejected_not_bug() {
        let v = classify::<()>(false, Err(revert()), || {}, "a", "b").unwrap();
        assert!(matches!(v, Verdict::Rejected { .. }));
    }

    #[test]
    fn unexpected_accept_is_bug() {
        let e = classify::<()>(
            false,
            Ok(AppResponse::evm((), Default::default(), vec![])),
            || {},
            "over-accept",
            "b",
        )
        .unwrap_err();
        assert!(matches!(e, HarnessError::Bug(m) if m.contains("over-accept")));
    }

    #[test]
    fn unexpected_revert_is_bug() {
        let e = classify::<()>(true, Err(revert()), || {}, "a", "valid reverted").unwrap_err();
        assert!(matches!(e, HarnessError::Bug(m) if m.contains("valid reverted")));
    }

    #[test]
    fn non_execute_error_is_infra() {
        let e =
            classify::<()>(true, Err(CrossVmError::wallet("boom")), || {}, "a", "b").unwrap_err();
        assert!(matches!(e, HarnessError::Infra(_)));
    }
}
