use crate::brokers::PodWatchEvent;

/// Apply one pod-watch event to the cache. Returns true when the event was a
/// full snapshot (`Restarted`), which is what marks the watch as ready.

pub(crate) fn apply_watch_event(
    cache: &mut std::collections::HashMap<String, k8s_openapi::api::core::v1::Pod>,
    event: PodWatchEvent,
) -> bool {
    match event {
        PodWatchEvent::Applied(pod) => {
            if let Some(uid) = pod.metadata.uid.clone() {
                cache.insert(uid, pod);
            }
            false
        }
        PodWatchEvent::Deleted(uid) => {
            cache.remove(&uid);
            false
        }
        PodWatchEvent::Restarted(pods) => {
            cache.clear();
            for pod in pods {
                if let Some(uid) = pod.metadata.uid.clone() {
                    cache.insert(uid, pod);
                }
            }
            true
        }
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    fn pod(uid: &str) -> Pod {
        Pod {
            metadata: ObjectMeta {
                uid: Some(uid.into()),
                name: Some(format!("pod-{uid}")),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn applied_event_inserts_pod() {
        let mut cache = std::collections::HashMap::new();
        assert!(!apply_watch_event(
            &mut cache,
            PodWatchEvent::Applied(pod("u1"))
        ));
        assert!(cache.contains_key("u1"));
    }

    #[test]
    fn deleted_event_removes_pod() {
        let mut cache = std::collections::HashMap::new();
        apply_watch_event(&mut cache, PodWatchEvent::Applied(pod("u1")));
        assert!(!apply_watch_event(
            &mut cache,
            PodWatchEvent::Deleted("u1".into())
        ));
        assert!(cache.is_empty());
    }

    #[test]
    fn restarted_event_replaces_cache_and_marks_ready() {
        let mut cache = std::collections::HashMap::new();
        apply_watch_event(&mut cache, PodWatchEvent::Applied(pod("old")));
        let snapshot = apply_watch_event(
            &mut cache,
            PodWatchEvent::Restarted(vec![pod("a"), pod("b")]),
        );
        assert!(snapshot);
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("a") && cache.contains_key("b"));
    }
}
