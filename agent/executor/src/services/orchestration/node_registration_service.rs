use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, instrument, warn};

use crate::brokers::ControlPlaneBroker;
use crate::error::Result;
use crate::services::foundation::node_capability_service::NodeCapabilityService;

/// How often a status update is pushed. kubelet's own default is 10 s and k3s
/// marks the node NotReady after 40 s of silence, so a 30 s interval only ever
/// tolerated a single lost beat - one short sleep was enough to drop the node.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// How often the loop wakes to look for a pending nudge. It only compares an
/// `Instant`, so this is cheaper than any timer granularity concern.
const HEARTBEAT_TICK: Duration = Duration::from_secs(2);

// ─── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait NodeRegistrationService: Send + Sync {
    /// Register this device as a node and start the heartbeat loop.
    async fn run(&self) -> Result<()>;
    /// Gracefully deregister.
    async fn deregister(&self) -> Result<()>;
}

// ─── Implementation ───────────────────────────────────────────────────────────

pub struct NodeRegistrationServiceImpl {
    cp_broker: Arc<dyn ControlPlaneBroker>,
    capability_service: Arc<dyn NodeCapabilityService>,
}

impl NodeRegistrationServiceImpl {
    pub fn new(
        cp_broker: Arc<dyn ControlPlaneBroker>,
        capability_service: Arc<dyn NodeCapabilityService>,
    ) -> Self {
        Self {
            cp_broker,
            capability_service,
        }
    }
}

#[async_trait]
impl NodeRegistrationService for NodeRegistrationServiceImpl {
    #[instrument(skip(self))]
    async fn run(&self) -> Result<()> {
        // 1. Register
        let profile = self.capability_service.get_profile().await?;
        info!(node_id = %profile.node_id, "registering node with control plane");
        self.cp_broker.register_node(&profile).await?;
        info!(node_id = %profile.node_id, "node registered");

        // 2. Heartbeat loop
        //
        // Sleeps in short ticks instead of one long interval so a nudge from the
        // telemetry socket (device just woke up) turns into an immediate status
        // update. `last_sent` starts as `None` so the first beat goes out right
        // after registration rather than 10 s later.
        let mut last_sent: Option<Instant> = None;
        loop {
            tokio::time::sleep(HEARTBEAT_TICK).await;

            let nudged = crate::status_ipc::take_heartbeat_request();
            let due = match last_sent {
                None => true,
                Some(t) => t.elapsed() >= HEARTBEAT_INTERVAL,
            };
            if !nudged && !due {
                continue;
            }
            // Claim the slot before doing the work: a failing profile read or a
            // failing send must not turn into a 2 s retry storm.
            last_sent = Some(Instant::now());

            let profile = match self.capability_service.get_profile().await {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "failed to read node profile for heartbeat");
                    continue;
                }
            };

            // Keep a copy for the 404 re-registration path (fields below move).
            let profile_for_register = profile.clone();
            let status = crate::models::NodeStatus {
                node_id: profile.node_id.clone(),
                conditions: profile.conditions,
                memory: profile.memory,
                storage: profile.storage,
                battery: profile.battery,
                network: profile.network,
                images: profile.images,
            };

            if let Err(e) = self.cp_broker.send_heartbeat(&status).await {
                error!(error = %e, "heartbeat failed");
                // `kubectl delete node` removes the object while the agent keeps
                // running. A status patch then returns 404 forever, so the node
                // never comes back until the app is restarted. Re-create it.
                if is_not_found(&e) {
                    warn!(
                        node = %status.node_id,
                        "node object disappeared; re-registering"
                    );
                    match self.cp_broker.register_node(&profile_for_register).await {
                        Ok(()) => info!(node = %status.node_id, "node re-registered"),
                        Err(e2) => error!(error = %e2, "node re-registration failed"),
                    }
                }
            }
        }
    }

    #[instrument(skip(self))]
    async fn deregister(&self) -> Result<()> {
        info!("deregistering node from control plane");
        self.cp_broker.deregister_node().await
    }
}

/// True when the control plane says the Node object no longer exists.
fn is_not_found(error: &crate::error::DroidError) -> bool {
    matches!(
        error,
        crate::error::DroidError::KubeApi(kube::Error::Api(api)) if api.code == 404
    )
}
