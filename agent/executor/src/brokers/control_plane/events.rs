use async_trait::async_trait;
use k8s_openapi::api::core::v1::Event;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};
use tracing::instrument;

use crate::error::{DroidError, Result};
use crate::models::NodeEvent;

use super::{ControlPlaneBrokerImpl, EventRecorder};

#[async_trait]
impl EventRecorder for ControlPlaneBrokerImpl {
    #[instrument(skip(self, event), fields(node = %self.node_name))]
    async fn record_event(&self, event: &NodeEvent) -> Result<()> {
        use chrono::Utc;

        let namespace = event
            .pod
            .as_ref()
            .map(|p| p.namespace.clone())
            .unwrap_or_else(|| "default".to_string());
        let events: Api<Event> = Api::namespaced(self.client.clone(), &namespace);

        let event_type = match event.event_type {
            crate::models::EventType::Normal => "Normal",
            crate::models::EventType::Warning => "Warning",
        };

        // involvedObject is what makes `kubectl describe pod` / `kubectl get
        // events --field-selector involvedObject.name=...` find the event.
        let involved = event
            .pod
            .as_ref()
            .map(|p| k8s_openapi::api::core::v1::ObjectReference {
                api_version: Some("v1".to_string()),
                kind: Some("Pod".to_string()),
                name: Some(p.name.clone()),
                namespace: Some(p.namespace.clone()),
                uid: p.uid.clone(),
                ..Default::default()
            });
        let pod_name = event.pod.as_ref().map(|p| p.name.clone());

        let ev = Event {
            metadata: ObjectMeta {
                // Prefer a human-readable name so `kubectl get events` is legible.
                name: pod_name
                    .as_ref()
                    .map(|n| format!("{}.{:x}", n, Utc::now().timestamp_millis())),
                generate_name: if pod_name.is_none() {
                    Some(format!("{}-", self.node_name))
                } else {
                    None
                },
                namespace: Some(namespace.clone()),
                ..Default::default()
            },
            involved_object: involved.unwrap_or_default(),
            event_time: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime(
                Utc::now(),
            )),
            action: Some(event.reason.clone()),
            reason: Some(event.reason.clone()),
            message: Some(event.message.clone()),
            type_: Some(event_type.into()),
            reporting_component: Some("droidnode".into()),
            reporting_instance: Some(self.node_name.clone()),
            ..Default::default()
        };

        events
            .create(&PostParams::default(), &ev)
            .await
            .map_err(DroidError::KubeApi)?;

        Ok(())
    }
}
