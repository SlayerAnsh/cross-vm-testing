//! [`classify`]: collapse the four-way (expected, result) match on a cross-VM response.

use cross_vm_core::CrossVmError;
use harness_core::{HarnessError, Verdict};

use crate::contract::AppResponse;

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
        Err(e) => Err(HarnessError::infra(e)),
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
