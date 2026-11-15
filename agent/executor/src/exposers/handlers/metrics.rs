use axum::{extract::State, http::StatusCode, response::IntoResponse};

use crate::exposers::state::*;

pub(crate) async fn metrics(State(state): State<KubeletState>) -> impl IntoResponse {
    let mut out = String::new();

    // E1/E12: real device telemetry pushed by the app (battery + temperature +
    // network). Before this the endpoint emitted nothing at all because
    // `node_profile` was never populated.
    if let Some(device) = state.device_state() {
        let telemetry = device.snapshot().await;
        if telemetry.is_fresh() {
            if let Some(percent) = telemetry.battery_percent {
                out.push_str(&format!(
                    "# HELP droidnode_battery_percent Battery charge percentage\n\
                     # TYPE droidnode_battery_percent gauge\n\
                     droidnode_battery_percent {percent}\n"
                ));
            }
            if let Some(charging) = telemetry.battery_charging {
                out.push_str(&format!(
                    "# HELP droidnode_battery_charging 1 when the device is charging\n\
                     # TYPE droidnode_battery_charging gauge\n\
                     droidnode_battery_charging {}\n",
                    if charging { 1 } else { 0 }
                ));
            }
            if let Some(celsius) = telemetry.battery_temperature_c {
                out.push_str(&format!(
                    "# HELP droidnode_battery_temperature_celsius Battery temperature\n\
                     # TYPE droidnode_battery_temperature_celsius gauge\n\
                     droidnode_battery_temperature_celsius {celsius}\n"
                ));
            }
            if let Some(kind) = telemetry.network_type.as_deref() {
                out.push_str(&format!(
                    "# HELP droidnode_network_up 1 when a network is available\n\
                     # TYPE droidnode_network_up gauge\n\
                     droidnode_network_up {}\n",
                    if kind == "none" || kind.is_empty() {
                        0
                    } else {
                        1
                    }
                ));
            }
        }
    }

    let memory = crate::services::foundation::node_capability_service::read_memory_info();
    out.push_str(&format!(
        "# HELP droidnode_memory_available_bytes Available memory in bytes\n\
         # TYPE droidnode_memory_available_bytes gauge\n\
         droidnode_memory_available_bytes {}\n",
        memory.available_bytes
    ));
    out.push_str(&format!(
        "# HELP droidnode_memory_total_bytes Total memory in bytes\n\
         # TYPE droidnode_memory_total_bytes gauge\n\
         droidnode_memory_total_bytes {}\n",
        memory.total_bytes
    ));

    let storage =
        crate::services::foundation::node_capability_service::read_storage_info(&state.rootfs_base);
    out.push_str(&format!(
        "# HELP droidnode_storage_available_bytes Available storage in bytes\n\
         # TYPE droidnode_storage_available_bytes gauge\n\
         droidnode_storage_available_bytes {}\n",
        storage.available_bytes
    ));
    out.push_str(&format!(
        "# HELP droidnode_storage_total_bytes Total storage in bytes\n\
         # TYPE droidnode_storage_total_bytes gauge\n\
         droidnode_storage_total_bytes {}\n",
        storage.total_bytes
    ));

    let pods = crate::status_ipc::running_pods(&state).await;
    out.push_str(&format!(
        "# HELP droidnode_pods_running Pods with at least one running container\n\
         # TYPE droidnode_pods_running gauge\n\
         droidnode_pods_running {pods}\n"
    ));

    (
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4")],
        out,
    )
}
