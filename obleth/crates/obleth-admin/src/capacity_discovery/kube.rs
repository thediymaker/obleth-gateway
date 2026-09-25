//! The one Kubernetes call capacity discovery makes: list the pods a label
//! selector matches in a namespace.
//!
//! Plain `reqwest` against the API server rather than a Kubernetes client
//! crate: one read-only endpoint does not justify the `kube`/`k8s-openapi`
//! dependency tree and its compile time, and the gateway already ships
//! `reqwest` with rustls. In a pod the client uses the mounted ServiceAccount
//! (API server from `KUBERNETES_SERVICE_HOST`/`_PORT`, the cluster CA, and the
//! token re-read on every call, since projected tokens rotate). Only the pod
//! fields discovery needs are deserialized; nothing read here is logged.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Per-call timeout. Well under the default discovery interval.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Pods per page when the API server pages the list.
const PAGE_LIMIT: u32 = 500;

/// A minimal read-only Kubernetes API client.
#[derive(Clone)]
pub struct KubeClient {
    http: reqwest::Client,
    base: String,
    token: Token,
}

#[derive(Clone)]
enum Token {
    /// Re-read from this file on every request.
    File(PathBuf),
    Static(Option<String>),
}

impl KubeClient {
    /// The in-cluster client: the API server named by the pod's environment,
    /// trusted through the ServiceAccount's CA bundle.
    pub fn in_cluster() -> Result<Self, String> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .ok()
            .filter(|h| !h.trim().is_empty())
            .ok_or("not running in a Kubernetes pod (KUBERNETES_SERVICE_HOST is unset)")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "443".into());
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        let dir = PathBuf::from(SERVICE_ACCOUNT_DIR);
        let ca = std::fs::read(dir.join("ca.crt")).map_err(|e| {
            format!(
                "cannot read the ServiceAccount CA ({e}); is a service account token \
                 mounted into the gateway pod?"
            )
        })?;
        let ca = reqwest::Certificate::from_pem(&ca)
            .map_err(|e| format!("invalid ServiceAccount CA: {e}"))?;
        let http = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("cannot build the Kubernetes client: {e}"))?;
        Ok(KubeClient {
            http,
            base: format!("https://{host}:{port}"),
            token: Token::File(dir.join("token")),
        })
    }

    /// A client for an explicit API base URL, for tests against a fake API
    /// server.
    pub fn with_base(base: impl Into<String>, token: Option<String>) -> Self {
        KubeClient {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("static client config"),
            base: base.into().trim_end_matches('/').to_string(),
            token: Token::Static(token),
        }
    }

    fn bearer(&self) -> Result<Option<String>, String> {
        match &self.token {
            Token::Static(t) => Ok(t.clone()),
            Token::File(path) => std::fs::read_to_string(path)
                .map(|t| Some(t.trim().to_string()))
                .map_err(|e| format!("cannot read the ServiceAccount token: {e}")),
        }
    }

    /// Every pod in `namespace` that `selector` matches. The first page is
    /// asked for at `resourceVersion=0`, which the API server may answer from
    /// its watch cache instead of etcd.
    pub async fn list_pods(&self, namespace: &str, selector: &str) -> Result<Vec<Pod>, String> {
        let mut out = Vec::new();
        let mut cont: Option<String> = None;
        loop {
            let mut url =
                reqwest::Url::parse(&format!("{}/api/v1/namespaces/{namespace}/pods", self.base))
                    .map_err(|e| format!("bad Kubernetes API URL: {e}"))?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("labelSelector", selector);
                q.append_pair("limit", &PAGE_LIMIT.to_string());
                match &cont {
                    Some(token) => {
                        q.append_pair("continue", token);
                    }
                    None => {
                        q.append_pair("resourceVersion", "0");
                    }
                }
            }
            let mut req = self.http.get(url).header("accept", "application/json");
            if let Some(token) = self.bearer()? {
                req = req.bearer_auth(token);
            }
            let res = req
                .send()
                .await
                .map_err(|e| format!("Kubernetes API unreachable: {e}"))?;
            let status = res.status();
            if !status.is_success() {
                let body = res.text().await.unwrap_or_default();
                let message = serde_json::from_str::<ApiStatus>(&body)
                    .ok()
                    .and_then(|s| s.message)
                    .unwrap_or_default();
                let hint = if status == reqwest::StatusCode::FORBIDDEN {
                    " (the gateway's ServiceAccount needs get/list on pods in this namespace)"
                } else {
                    ""
                };
                return Err(format!(
                    "listing pods in {namespace} failed: HTTP {}{}{hint}",
                    status.as_u16(),
                    if message.is_empty() {
                        String::new()
                    } else {
                        format!(": {message}")
                    },
                ));
            }
            let page: PodList = res
                .json()
                .await
                .map_err(|e| format!("unreadable pod list from {namespace}: {e}"))?;
            out.extend(page.items);
            match page.metadata.cont.filter(|c| !c.is_empty()) {
                Some(next) => cont = Some(next),
                None => return Ok(out),
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ApiStatus {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PodList {
    #[serde(default)]
    items: Vec<Pod>,
    #[serde(default)]
    metadata: ListMeta,
}

#[derive(Debug, Default, Deserialize)]
struct ListMeta {
    #[serde(rename = "continue", default)]
    cont: Option<String>,
}

/// The parts of a pod discovery reads.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pod {
    #[serde(default)]
    pub metadata: PodMeta,
    #[serde(default)]
    pub spec: PodSpec,
    #[serde(default)]
    pub status: PodStatus,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PodMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(rename = "deletionTimestamp", default)]
    pub deletion_timestamp: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PodSpec {
    #[serde(default)]
    pub containers: Vec<Container>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Container {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EnvVar {
    pub name: String,
    /// Only a literal value; `valueFrom` is never read.
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PodStatus {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub conditions: Vec<PodCondition>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PodCondition {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
}

impl Pod {
    /// Serving: Running, its Ready condition True, and not being deleted. A
    /// terminating pod can stay Ready through its grace period while it
    /// drains, so it no longer counts.
    pub fn is_serving(&self) -> bool {
        self.metadata.deletion_timestamp.is_none()
            && self.status.phase == "Running"
            && self
                .status
                .conditions
                .iter()
                .any(|c| c.kind == "Ready" && c.status == "True")
    }
}
