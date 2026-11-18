use std::time::Duration;

use crate::models::PodPhase;

const BACKOFF_BASE_SECS: u64 = 10;
const BACKOFF_MAX_SECS: u64 = 300;

pub(super) fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(6);
    let secs = (BACKOFF_BASE_SECS << shift).min(BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

/// Decide whether a finished container should be restarted, per the pod's
/// `restartPolicy`. Pure so the policy matrix is unit-testable.
pub(crate) fn want_restart(policy: &str, phase: PodPhase) -> bool {
    match policy {
        "Always" => true,
        "OnFailure" => phase == PodPhase::Failed,
        _ => false,
    }
}

/// Grace window actually used when a pod is deleted: the one the API server
/// granted wins over the pod template's default.
pub(crate) fn effective_grace(pod_grace: Option<i64>, workload_grace: u64) -> u64 {
    pod_grace
        .filter(|g| *g >= 0)
        .map(|g| g as u64)
        .unwrap_or(workload_grace)
}

/// A pod that already finished is mirrored (not re-run) only for `Never`;
/// `Always`/`OnFailure` pods marked terminal by an older agent are re-created.
pub(crate) fn terminal_for_policy(policy: &str, k8s_phase: &str) -> bool {
    policy == "Never" && (k8s_phase == "Succeeded" || k8s_phase == "Failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_restarts_in_every_phase() {
        for phase in [PodPhase::Succeeded, PodPhase::Failed, PodPhase::Unknown] {
            assert!(
                want_restart("Always", phase.clone()),
                "Always must restart in {phase:?}"
            );
        }
    }

    #[test]
    fn on_failure_restarts_only_after_failure() {
        assert!(want_restart("OnFailure", PodPhase::Failed));
        assert!(!want_restart("OnFailure", PodPhase::Succeeded));
        assert!(!want_restart("OnFailure", PodPhase::Running));
    }

    #[test]
    fn never_never_restarts() {
        for phase in [PodPhase::Succeeded, PodPhase::Failed] {
            assert!(!want_restart("Never", phase.clone()));
        }
    }

    #[test]
    fn api_server_grace_wins_over_pod_default() {
        assert_eq!(effective_grace(Some(5), 30), 5);
        assert_eq!(effective_grace(Some(0), 30), 0);
        assert_eq!(effective_grace(None, 30), 30);
        // Negative values are invalid and fall back to the template default.
        assert_eq!(effective_grace(Some(-1), 30), 30);
    }

    #[test]
    fn terminal_only_for_never_policy() {
        assert!(terminal_for_policy("Never", "Succeeded"));
        assert!(terminal_for_policy("Never", "Failed"));
        assert!(!terminal_for_policy("Always", "Failed"));
        assert!(!terminal_for_policy("Never", "Running"));
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_delay(1), Duration::from_secs(10));
        assert_eq!(backoff_delay(2), Duration::from_secs(20));
        assert_eq!(backoff_delay(3), Duration::from_secs(40));
        assert_eq!(backoff_delay(50), Duration::from_secs(BACKOFF_MAX_SECS));
    }
}
