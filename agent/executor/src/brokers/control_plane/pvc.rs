use async_trait::async_trait;
use k8s_openapi::api::core::v1::{
    HostPathVolumeSource, ObjectReference, PersistentVolume, PersistentVolumeClaim,
    PersistentVolumeSpec, Service,
};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::storage::v1::StorageClass;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};
use tracing::{debug, info, instrument, warn};

use crate::error::{DroidError, Result};

use super::{service_info, ControlPlaneBrokerImpl, EndpointInfo, PvcApi, ServiceInfo};

#[async_trait]
impl PvcApi for ControlPlaneBrokerImpl {
    #[instrument(skip(self), fields(ns = %namespace, name = %name))]
    async fn get_config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, String>> {
        use k8s_openapi::api::core::v1::ConfigMap;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let cm = api.get(name).await.map_err(DroidError::KubeApi)?;
        Ok(cm.data.unwrap_or_default())
    }

    #[instrument(skip(self), fields(ns = %namespace, name = %name))]
    async fn get_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
        use k8s_openapi::api::core::v1::Secret;

        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let secret = api.get(name).await.map_err(DroidError::KubeApi)?;

        let mut out = std::collections::BTreeMap::new();
        if let Some(data) = secret.data {
            for (k, v) in data {
                out.insert(k, v.0.clone());
            }
        }
        // `stringData` is write-only in the API, but decode it if present anyway.
        if let Some(string_data) = secret.string_data {
            for (k, v) in string_data {
                out.insert(k, v.into_bytes());
            }
        }
        // k8s-openapi gives us the already-decoded `ByteString` values; no
        // base64 handling is needed here.
        Ok(out)
    }

    #[instrument(skip(self), fields(ns = %namespace))]
    async fn list_services(&self, namespace: &str) -> Result<Vec<ServiceInfo>> {
        let api: Api<Service> = Api::namespaced(self.client.clone(), namespace);
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        Ok(list.items.into_iter().filter_map(service_info).collect())
    }

    async fn list_all_services(&self) -> Result<Vec<ServiceInfo>> {
        let api: Api<Service> = Api::all(self.client.clone());
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        Ok(list.items.into_iter().filter_map(service_info).collect())
    }

    async fn list_ready_endpoints(&self) -> Result<Vec<EndpointInfo>> {
        let api: Api<EndpointSlice> = Api::all(self.client.clone());
        let list = api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        let mut grouped: std::collections::HashMap<(String, String, i32), Vec<String>> =
            std::collections::HashMap::new();
        for slice in list.items {
            let Some(service) = slice
                .metadata
                .labels
                .as_ref()
                .and_then(|l| l.get("kubernetes.io/service-name"))
                .cloned()
            else {
                continue;
            };
            let namespace = slice.metadata.namespace.clone().unwrap_or_default();
            let Some(ports) = slice.ports else { continue };
            for port in ports {
                let Some(number) = port.port else { continue };
                for endpoint in &slice.endpoints {
                    if endpoint.conditions.as_ref().and_then(|c| c.ready) == Some(false) {
                        continue;
                    }
                    let entry = grouped
                        .entry((namespace.clone(), service.clone(), number))
                        .or_default();
                    for address in &endpoint.addresses {
                        if !entry.contains(address) {
                            entry.push(address.clone());
                        }
                    }
                }
            }
        }
        Ok(grouped
            .into_iter()
            .map(|((namespace, service, port), addresses)| EndpointInfo {
                namespace,
                service,
                port,
                addresses,
            })
            .collect())
    }

    async fn pvc_host_path(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>> {
        let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(self.client.clone(), namespace);
        let pvc = match pvc_api.get(name).await {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let Some(volume_name) = pvc.spec.and_then(|s| s.volume_name) else {
            return Ok(None);
        };
        let pv_api: Api<PersistentVolume> = Api::all(self.client.clone());
        let pv = match pv_api.get(&volume_name).await {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        Ok(pv
            .spec
            .and_then(|s| s.host_path)
            .map(|h| std::path::PathBuf::from(h.path)))
    }

    async fn reconcile_local_pvcs(&self, base: &std::path::Path) -> Result<()> {
        let pv_api: Api<PersistentVolume> = Api::all(self.client.clone());
        let pvc_api: Api<PersistentVolumeClaim> = Api::all(self.client.clone());
        let sc_api: Api<StorageClass> = Api::all(self.client.clone());

        if sc_api.get("droidnode-local").await.is_err() {
            let sc = StorageClass {
                metadata: ObjectMeta {
                    name: Some("droidnode-local".into()),
                    ..Default::default()
                },
                provisioner: "droidnode.io/local".into(),
                reclaim_policy: Some("Retain".into()),
                volume_binding_mode: Some("Immediate".into()),
                ..Default::default()
            };
            if let Err(e) = sc_api.create(&PostParams::default(), &sc).await {
                debug!(error = %e, "B10: could not create StorageClass (may already exist)");
            }
        }

        let list = pvc_api
            .list(&Default::default())
            .await
            .map_err(DroidError::KubeApi)?;
        for pvc in list.items {
            if pvc
                .spec
                .as_ref()
                .and_then(|s| s.storage_class_name.as_deref())
                != Some("droidnode-local")
            {
                continue;
            }
            let Some(uid) = pvc.metadata.uid.clone() else {
                continue;
            };
            let namespace = pvc.metadata.namespace.clone().unwrap_or_default();
            let pvc_name = pvc.metadata.name.clone().unwrap_or_default();
            let dir = base.join(&uid);
            tokio::fs::create_dir_all(&dir).await?;

            let pv_name = format!("droidnode-{uid}");
            if pv_api.get(&pv_name).await.is_ok() {
                continue;
            }
            let spec = PersistentVolumeSpec {
                capacity: Some(
                    [("storage".to_string(), Quantity("1Gi".to_string()))]
                        .into_iter()
                        .collect(),
                ),
                access_modes: Some(vec!["ReadWriteOnce".to_string()]),
                persistent_volume_reclaim_policy: Some("Retain".to_string()),
                storage_class_name: Some("droidnode-local".to_string()),
                host_path: Some(HostPathVolumeSource {
                    path: dir.to_string_lossy().to_string(),
                    type_: Some("DirectoryOrCreate".to_string()),
                }),
                claim_ref: Some(ObjectReference {
                    api_version: Some("v1".into()),
                    kind: Some("PersistentVolumeClaim".into()),
                    namespace: Some(namespace.clone()),
                    name: Some(pvc_name.clone()),
                    uid: Some(uid.clone()),
                    ..Default::default()
                }),
                volume_mode: Some("Filesystem".into()),
                ..Default::default()
            };
            let pv = PersistentVolume {
                metadata: ObjectMeta {
                    name: Some(pv_name.clone()),
                    ..Default::default()
                },
                spec: Some(spec),
                ..Default::default()
            };
            match pv_api.create(&PostParams::default(), &pv).await {
                Ok(_) => info!(
                    pv = %pv_name,
                    pvc = %format!("{namespace}/{pvc_name}"),
                    path = %dir.display(),
                    "B10: created local PV"
                ),
                Err(e) => warn!(pv = %pv_name, error = %e, "B10: PV create failed"),
            }
        }
        Ok(())
    }
}
