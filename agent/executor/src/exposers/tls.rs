use k8s_openapi::api::certificates::v1::{
    CertificateSigningRequest, CertificateSigningRequestSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::ByteString;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::Client;

use crate::error::Result;

// ─── TLS config ───────────────────────────────────────────────────────────────

/// Build a rustls ServerConfig for the kubelet HTTPS endpoint.
///
/// Generates a persistent CA on first run and saves it to
/// `$DROIDNODE_DATA_DIR/kubelet-ca.{crt,key}` (default: ~/.droidnode/).
/// The kubelet serving cert is signed by that CA on every startup.
///
/// On first run the agent prints the CA cert path; add it to k3s once:
///   kube-apiserver-arg:
///     - "kubelet-certificate-authority=<path>"
/// After that one restart k3s trusts every cert we sign.
/// A16: obtain a kubelet serving cert signed by the cluster CA through the
/// `kubernetes.io/kubelet-serving` CSR signer. Returns None when the API/RBAC
/// or signer is unavailable, in which case the caller falls back to the
/// device-local CA.
pub(crate) async fn try_csr_serving_cert(
    sans: &[String],
    node: &str,
) -> Option<(
    Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
    tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
)> {
    use rcgen::{CertificateParams, DnType, KeyPair};

    if node.is_empty() {
        return None;
    }
    let key = KeyPair::generate().ok()?;
    let mut params = CertificateParams::new(sans.to_vec()).ok()?;
    params
        .distinguished_name
        .push(DnType::CommonName, format!("system:node:{node}"));
    params
        .distinguished_name
        .push(DnType::OrganizationName, "system:nodes");
    params.is_ca = rcgen::IsCa::NoCa;
    let csr = params.serialize_request(&key).ok()?;

    let client = Client::try_default().await.ok()?;
    let api: Api<CertificateSigningRequest> = Api::all(client);
    let name = format!("droidnode-kubelet-{node}");
    if api.get(&name).await.is_ok() {
        let _ = api.delete(&name, &Default::default()).await;
    }
    let request = CertificateSigningRequest {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            ..Default::default()
        },
        spec: CertificateSigningRequestSpec {
            request: ByteString(csr.pem().ok()?.into_bytes()),
            signer_name: "kubernetes.io/kubelet-serving".into(),
            usages: Some(vec![
                "server auth".into(),
                "digital signature".into(),
                "key encipherment".into(),
            ]),
            username: Some(format!("system:node:{node}")),
            groups: Some(vec!["system:nodes".into()]),
            ..Default::default()
        },
        ..Default::default()
    };
    if let Err(e) = api.create(&PostParams::default(), &request).await {
        tracing::warn!(error = %e, "A16: could not create serving CSR");
        return None;
    }
    let approval = serde_json::json!({
        "status": { "conditions": [{
            "type": "Approved",
            "status": "True",
            "reason": "DroidNodeBootstrap",
            "message": "auto-approved by node agent"
        }]}
    });
    if let Err(e) = api
        .patch_approval(&name, &PatchParams::default(), &Patch::Merge(&approval))
        .await
    {
        tracing::warn!(error = %e, "A16: could not approve serving CSR");
        return None;
    }
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let got = match api.get(&name).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let Some(cert) = got.status.and_then(|s| s.certificate) else {
            continue;
        };
        let pem = String::from_utf8_lossy(&cert.0).to_string();
        let certs = parse_pem_certs(&pem);
        if !certs.is_empty() {
            tracing::info!(csr = %name, certs = certs.len(), "A16: serving cert issued by cluster CA");
            return Some((
                certs,
                tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            ));
        }
    }
    tracing::warn!(csr = %name, "A16: CSR was not signed in time");
    None
}

pub(crate) fn parse_pem_certs(
    pem: &str,
) -> Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>> {
    use base64::Engine;
    use tokio_rustls::rustls::pki_types::CertificateDer;

    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let Some(end) = after.find("-----END CERTIFICATE-----") else {
            break;
        };
        let body: String = after[..end].lines().map(str::trim).collect();
        rest = &after[end + "-----END CERTIFICATE-----".len()..];
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body) {
            out.push(CertificateDer::from(der));
        }
    }
    out
}

pub(crate) fn make_tls_config(
    sans: &[String],
    preissued: Option<(
        Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
        tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
    )>,
) -> Result<tokio_rustls::rustls::ServerConfig> {
    use base64::Engine;
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    use tokio_rustls::rustls;

    // Resolve data dir (same logic as Config::from_env in main.rs)
    let data_dir = crate::config::get().data_dir();
    let _ = std::fs::create_dir_all(&data_dir);

    let ca_cert_path = data_dir.join("kubelet-ca.crt");
    let ca_key_path = data_dir.join("kubelet-ca.key");

    // CA params are identical every run — same Subject DN → same Issuer field
    // in every cert we sign → k3s can match against the persisted CA cert.
    //
    // EVERY field must be deterministic: rcgen assigns a RANDOM serial and
    // now±1h validity by default, so re-deriving the CA on each start used to
    // produce a *different* certificate than the one on disk. kube-apiserver
    // then verified our serving cert against a CA it had never signed with and
    // every `kubectl logs` request died with 502 Bad Gateway.
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // Go (kube-apiserver) rejects a CA whose KeyUsage omits keyCertSign as soon
    // as the extension is present; emitting the standard set is what a real
    // kubelet CA looks like. OpenSSL is far more forgiving, so this only shows
    // up as `BadCertificate` from the apiserver.
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "droidnode-kubelet-ca");
    ca_params.serial_number = Some(rcgen::SerialNumber::from(vec![0x01]));
    ca_params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    ca_params.not_after = rcgen::date_time_ymd(2120, 1, 1);

    // Load or generate the CA key (persisted across restarts so the public key
    // stored in kubelet-ca.crt never changes after the first run).
    let ca_key = if ca_key_path.exists() {
        let pem = std::fs::read_to_string(&ca_key_path)
            .map_err(|e| crate::error::DroidError::Config(format!("CA key read: {e}")))?;
        KeyPair::from_pem(&pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA key parse: {e}")))?
    } else {
        KeyPair::generate()
            .map_err(|e| crate::error::DroidError::Config(format!("CA key gen: {e}")))?
    };

    // Derive (or re-derive) the CA Certificate object from the stable params+key.
    // rcgen 0.13: self_signed / signed_by return Certificate (not CertifiedKey).
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| crate::error::DroidError::Config(format!("CA self-sign: {e}")))?;

    // Persist the CA material. Compare the *public key* rather than the PEM:
    // rcgen signs the CA with ECDSA, whose signatures are randomized, so the DER
    // bytes differ on every start even though the trust anchor (key + subject)
    // is identical. The old byte comparison printed "CA changed, refresh the k3s
    // bundle" on every restart, which is both noisy and wrong.
    if !ca_key_path.exists() {
        std::fs::write(&ca_key_path, ca_key.serialize_pem())
            .map_err(|e| crate::error::DroidError::Config(format!("CA key write: {e}")))?;
    }
    let ca_pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64::engine::general_purpose::STANDARD.encode(ca_cert.der())
    );
    let pub_path = data_dir.join("kubelet-ca.pub");
    let pub_pem = ca_key.public_key_pem();
    let on_disk_pub = std::fs::read_to_string(&pub_path).unwrap_or_default();
    let pub_changed = on_disk_pub.trim() != pub_pem.trim();
    if pub_changed || !ca_cert_path.exists() {
        std::fs::write(&ca_cert_path, &ca_pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA cert write: {e}")))?;
        std::fs::write(&pub_path, &pub_pem)
            .map_err(|e| crate::error::DroidError::Config(format!("CA pub write: {e}")))?;
        tracing::warn!(
            "kubelet CA changed (new key) at {path} - refresh the k3s bundle:\n  \
            cat /var/lib/rancher/k3s/server/tls/server-ca.crt <this file> \
            > /etc/rancher/k3s/kubelet-ca-bundle.crt\n  \
            then: sudo systemctl restart k3s   (and re-copy it off the device)",
            path = ca_cert_path.display()
        );
    } else {
        tracing::debug!("kubelet CA unchanged (same public key)");
    }

    // A16: prefer a serving cert signed by the cluster CA via a CSR; fall
    // back to the device-local CA only when CSR bootstrap is unavailable.
    let (cert_chain, key_der) = if let Some((certs, key)) = preissued {
        tracing::info!("kubelet serving cert issued by cluster CA (CSR)");
        (certs, key)
    } else {
        // Sign the kubelet serving cert with our CA.
        // The IP must be a SAN — k3s connects to the InternalIP we advertise.
        // A distinct CN keeps the leaf from looking self-signed to verifiers.
        let mut server_params = CertificateParams::new(sans.to_vec())
            .map_err(|e| crate::error::DroidError::Config(format!("cert params: {e}")))?;
        server_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "droidnode-kubelet");
        // Standard server-certificate extensions. Without these the cert is a bare
        // public key + SAN, which OpenSSL accepts but Go (and therefore
        // kube-apiserver) refuses during the handshake.
        server_params.is_ca = rcgen::IsCa::NoCa;
        server_params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ];
        server_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate()
            .map_err(|e| crate::error::DroidError::Config(format!("server key gen: {e}")))?;
        let signed = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .map_err(|e| crate::error::DroidError::Config(format!("sign: {e}")))?;
        let cert_der = rustls::pki_types::CertificateDer::from(signed.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(server_key.serialize_der().into());
        (vec![cert_der], key_der)
    };

    tracing::info!(sans = ?sans, ca = %ca_cert_path.display(), "kubelet TLS ready");

    // A10: optionally require a client certificate signed by the cluster's
    // client CA (k3s: /var/lib/rancher/k3s/server/tls/client-ca.crt). It is
    // copied to <dataDir>/client-ca.crt (or $DROIDNODE_KUBELET_CLIENT_CA).
    // `allow_unauthenticated` keeps the local Android UI (no client cert)
    // working; the exec/attach/port-forward handlers enforce the cert.
    let client_ca = load_client_ca(&data_dir);
    let builder = rustls::ServerConfig::builder();
    let builder = if let Some(roots) = client_ca {
        let verifier = rustls::server::WebPkiClientVerifier::builder(roots)
            .allow_unauthenticated()
            .build()
            .map_err(|e| crate::error::DroidError::Config(format!("client CA: {e}")))?;
        tracing::info!("kubelet client-certificate verification enabled");
        builder.with_client_cert_verifier(verifier)
    } else {
        tracing::warn!(
            "no client CA configured - exec/attach/port-forward will refuse remote clients; \
             copy the cluster client-ca.crt to <dataDir>/client-ca.crt"
        );
        builder.with_no_client_auth()
    };
    builder
        .with_single_cert(cert_chain, key_der)
        .map_err(|e| crate::error::DroidError::Config(format!("tls: {e}")))
}

/// A10: load an optional client CA bundle for kubelet mTLS.
pub(crate) fn load_client_ca(
    data_dir: &std::path::Path,
) -> Option<std::sync::Arc<tokio_rustls::rustls::RootCertStore>> {
    use base64::Engine;
    use tokio_rustls::rustls::pki_types::CertificateDer;
    use tokio_rustls::rustls::RootCertStore;

    let path = crate::config::get()
        .kubelet_client_ca()
        .unwrap_or_else(|| data_dir.join("client-ca.crt"));
    let pem = std::fs::read_to_string(&path).ok()?;
    let mut roots = RootCertStore::empty();
    let mut rest = pem.as_str();
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let after = &rest[begin + "-----BEGIN CERTIFICATE-----".len()..];
        let Some(end) = after.find("-----END CERTIFICATE-----") else {
            break;
        };
        let body: String = after[..end].lines().map(str::trim).collect();
        rest = &after[end + "-----END CERTIFICATE-----".len()..];
        let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body) else {
            continue;
        };
        if roots.add(CertificateDer::from(der)).is_err() {
            continue;
        }
    }
    if roots.is_empty() {
        tracing::warn!(path = %path.display(), "client CA file has no usable certificates");
        return None;
    }
    tracing::info!(path = %path.display(), certs = roots.len(), "loaded kubelet client CA");
    Some(std::sync::Arc::new(roots))
}
