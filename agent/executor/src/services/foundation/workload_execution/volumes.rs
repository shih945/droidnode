use super::*;

impl WorkloadExecutionServiceImpl {
    /// B9: turn `pod.spec.volumes` into host directories and return
    /// `volume name -> host path`.
    ///
    /// Directory lives under `<rootfs parent>/volumes/<uid>/<name>` and is
    /// recreated on every start (emptyDir semantics).
    pub(super) async fn materialize_volumes(
        &self,
        pod: &Pod,
        rootfs: &Path,
    ) -> Result<std::collections::HashMap<String, PathBuf>> {
        let mut out = std::collections::HashMap::new();
        let Some(spec) = pod.spec.as_ref() else {
            return Ok(out);
        };
        let uid = pod.metadata.uid.clone().unwrap_or_else(|| "unknown".into());
        // Volumes belong to the POD, not to a container. Since every container now
        // has its own rootfs copy (B17) at <files>/pods/<uid>/<container>/rootfs,
        // deriving the directory from `rootfs.parent()` would give each container
        // a private emptyDir. Anchor at the "pods" component instead so all
        // containers of the pod (including init containers) share it.
        let base = {
            let mut found = None;
            for anc in rootfs.ancestors() {
                if anc.file_name().and_then(|n| n.to_str()) == Some("pods") {
                    found = Some(anc.join(&uid).join("volumes"));
                    break;
                }
            }
            found.unwrap_or_else(|| {
                rootfs
                    .parent()
                    .map(|p| p.join("volumes").join(&uid))
                    .unwrap_or_else(|| rootfs.join("volumes").join(&uid))
            })
        };
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".into());

        for volume in spec.volumes.iter().flatten() {
            let name = volume.name.clone();
            let dir = base.join(&name);

            if volume.empty_dir.is_some() {
                // NOTE: never wipe here. `materialize_volumes` runs once per
                // container, so removing the directory would erase what an init
                // container just wrote (and what the main container expects).
                // A fresh pod gets a fresh <uid> directory anyway.
                tokio::fs::create_dir_all(&dir).await?;
                out.insert(name, dir);
            } else if let Some(hp) = volume.host_path.as_ref() {
                // B12: a missing mount source is fatal instead of silently skipped.
                let path = resolve_host_path(&name, &hp.path)?;
                out.insert(name, path);
            } else if let Some(pvc) = volume.persistent_volume_claim.as_ref() {
                // B10: resolve the Bound PVC through its PV's hostPath.
                let claim = pvc.claim_name.clone();
                match self.cp_broker.pvc_host_path(&namespace, &claim).await {
                    Ok(Some(path)) => {
                        tokio::fs::create_dir_all(&path).await?;
                        out.insert(name, path);
                    }
                    Ok(None) => {
                        return Err(DroidError::Workload(format!(
                            "PVC {namespace}/{claim} is not Bound to a hostPath PV"
                        )))
                    }
                    Err(e) => {
                        return Err(DroidError::Workload(format!(
                            "PVC {namespace}/{claim}: {e}"
                        )))
                    }
                }
            } else if let Some(cm) = volume.config_map.as_ref() {
                let cm_name = cm.name.clone().unwrap_or_default();
                let data = self
                    .cp_broker
                    .get_config_map(&namespace, &cm_name)
                    .await
                    .map_err(|e| DroidError::Workload(format!("configMap \"{cm_name}\": {e}")))?;
                write_config_dir(&dir, &data).await?;
                for item in cm.items.iter().flatten() {
                    if let Some(v) = data.get(&item.key) {
                        let target = dir.join(&item.path);
                        if let Some(parent) = target.parent() {
                            let _ = tokio::fs::create_dir_all(parent).await;
                        }
                        let _ = tokio::fs::write(target, v).await;
                    }
                }
                out.insert(name, dir);
            } else if let Some(sec) = volume.secret.as_ref() {
                let sec_name = sec.secret_name.clone().unwrap_or_default();
                let data = self
                    .cp_broker
                    .get_secret(&namespace, &sec_name)
                    .await
                    .map_err(|e| DroidError::Workload(format!("secret \"{sec_name}\": {e}")))?;
                write_secret_dir(&dir, &data).await?;
                out.insert(name, dir);
            } else {
                tracing::debug!(volume = %name, "unsupported volume source - ignored");
            }
        }
        Ok(out)
    }

    /// B14: drop a service-account token the pods can use, reusing the agent's
    /// own credentials (the app cannot mint per-pod tokens without more RBAC).
    pub(super) async fn materialize_service_account(&self, rootfs: &Path) {
        let Ok(creds) = self.cp_broker.agent_credentials().await else {
            return;
        };
        let dir = rootfs.join("var/run/secrets/kubernetes.io/serviceaccount");
        if tokio::fs::create_dir_all(&dir).await.is_err() {
            return;
        }
        let _ = tokio::fs::write(dir.join("token"), creds.token).await;
        let _ = tokio::fs::write(dir.join("namespace"), creds.namespace).await;
        if !creds.ca_pem.is_empty() {
            let _ = tokio::fs::write(dir.join("ca.crt"), creds.ca_pem).await;
        }
    }

    /// B15/B16: host aliases and DNS config inside the container rootfs.
    ///
    /// These are written into the image rootfs, which is shared by every pod
    /// using that image - acceptable until per-pod writable layers (B17).
    pub(super) async fn prepare_rootfs_files(pod: &Pod, rootfs: &Path) {
        let etc = rootfs.join("etc");
        if tokio::fs::create_dir_all(&etc).await.is_err() {
            return;
        }

        // resolv.conf: cluster DNS + the standard search path.
        let dns = crate::config::get().cluster_dns().to_string();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let resolv = format!(
            "nameserver {dns}\nsearch {namespace}.svc.cluster.local svc.cluster.local cluster.local\noptions ndots:5\n"
        );
        let _ = tokio::fs::write(etc.join("resolv.conf"), resolv).await;

        // /etc/hosts: keep the base content, append hostAliases.
        let hosts_path = etc.join("hosts");
        let mut hosts = tokio::fs::read_to_string(&hosts_path)
            .await
            .unwrap_or_else(|_| "127.0.0.1 localhost\n::1 localhost\n".to_string());
        if let Some(aliases) = pod.spec.as_ref().and_then(|s| s.host_aliases.as_ref()) {
            for alias in aliases {
                let names = alias.hostnames.clone().unwrap_or_default().join(" ");
                if let Some(ip) = alias.ip.clone() {
                    if !names.is_empty() {
                        hosts.push_str(&format!("{ip} {names}\n"));
                    }
                }
            }
        }
        let _ = tokio::fs::write(&hosts_path, hosts).await;
    }
}
