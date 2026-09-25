//! The one kind of Kubernetes call capacity discovery makes: list
//! EndpointSlices in a namespace, those of one named Service or those of
//! every Service, and count their ready endpoints.
//!
//! Plain `reqwest` against the API server rather than a Kubernetes client
//! crate: one read-only endpoint does not justify the `kube`/`k8s-openapi`
//! dependency tree and its compile time, and the gateway already ships
//! `reqwest` with rustls. In a pod the client uses the mounted ServiceAccount
//! (API server from `KUBERNETES_SERVICE_HOST`/`_PORT`, the cluster CA, and the
//! token re-read on every call, since projected tokens rotate).
//!
//! Least privilege by construction: the only path this client can build is
//! `/apis/discovery.k8s.io/v1/namespaces/{ns}/endpointslices`, so the
//! gateway needs nothing but `get`/`list`/`watch` on `endpointslices` in the
//! `discovery.k8s.io` group. An EndpointSlice holds a Service's endpoint
//! addresses, ports, readiness and the name of the pod behind each endpoint,
//! never a pod spec, environment or Secret. Only the fields discovery counts
//! are deserialized, and nothing read here is logged.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// Per-call timeout. Well under the default discovery interval.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Slices per page when the API server pages the list. A Service has one
/// slice per 100 endpoints by default, so one page is the norm.
const PAGE_LIMIT: u32 = 500;

/// The label the EndpointSlice controller puts on every slice of a Service.
pub const SERVICE_NAME_LABEL: &str = "kubernetes.io/service-name";

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

    /// The URL of one page of `service`'s EndpointSlices in `namespace`.
    #[cfg(test)]
    pub(crate) fn endpoint_slices_url(
        &self,
        namespace: &str,
        service: &str,
        cont: Option<&str>,
    ) -> Result<reqwest::Url, String> {
        self.slices_url(namespace, &format!("{SERVICE_NAME_LABEL}={service}"), cont)
    }

    /// The URL of one page of EndpointSlices in `namespace` matching the
    /// label `selector`: the only request this client makes. The first page
    /// is asked for at `resourceVersion=0`, which the API server may answer
    /// from its watch cache instead of etcd.
    fn slices_url(
        &self,
        namespace: &str,
        selector: &str,
        cont: Option<&str>,
    ) -> Result<reqwest::Url, String> {
        let mut url = reqwest::Url::parse(&format!(
            "{}/apis/discovery.k8s.io/v1/namespaces/{namespace}/endpointslices",
            self.base
        ))
        .map_err(|e| format!("bad Kubernetes API URL: {e}"))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("labelSelector", selector);
            q.append_pair("limit", &PAGE_LIMIT.to_string());
            match cont {
                Some(token) => {
                    q.append_pair("continue", token);
                }
                None => {
                    q.append_pair("resourceVersion", "0");
                }
            }
        }
        Ok(url)
    }

    /// Every EndpointSlice of `service` in `namespace`. An empty list means
    /// the namespace has no such Service (or one without a selector whose
    /// slices nobody manages): the EndpointSlice controller keeps at least
    /// one slice, possibly with no endpoints, for every Service it manages.
    pub async fn list_endpoint_slices(
        &self,
        namespace: &str,
        service: &str,
    ) -> Result<Vec<EndpointSlice>, String> {
        self.list_slices(
            namespace,
            &format!("{SERVICE_NAME_LABEL}={service}"),
            &format!("Service {service}"),
        )
        .await
    }

    /// The EndpointSlices of every Service in `namespace`: those carrying
    /// the Service-name label, which the EndpointSlice controller sets.
    pub async fn list_service_endpoint_slices(
        &self,
        namespace: &str,
    ) -> Result<Vec<EndpointSlice>, String> {
        self.list_slices(namespace, SERVICE_NAME_LABEL, "the Services")
            .await
    }

    async fn list_slices(
        &self,
        namespace: &str,
        selector: &str,
        what: &str,
    ) -> Result<Vec<EndpointSlice>, String> {
        let mut out = Vec::new();
        let mut cont: Option<String> = None;
        loop {
            let url = self.slices_url(namespace, selector, cont.as_deref())?;
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
                    " (the gateway's ServiceAccount needs get/list/watch on endpointslices \
                     (discovery.k8s.io) in this namespace)"
                } else {
                    ""
                };
                return Err(format!(
                    "listing the EndpointSlices of {what} in {namespace} failed: \
                     HTTP {}{}{hint}",
                    status.as_u16(),
                    if message.is_empty() {
                        String::new()
                    } else {
                        format!(": {message}")
                    },
                ));
            }
            let page: EndpointSliceList = res.json().await.map_err(|e| {
                format!("unreadable EndpointSlice list for {what} in {namespace}: {e}")
            })?;
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
struct EndpointSliceList {
    #[serde(default)]
    items: Vec<EndpointSlice>,
    #[serde(default)]
    metadata: ListMeta,
}

#[derive(Debug, Default, Deserialize)]
struct ListMeta {
    #[serde(rename = "continue", default)]
    cont: Option<String>,
}

/// The parts of an EndpointSlice discovery reads.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EndpointSlice {
    #[serde(default)]
    pub metadata: SliceMeta,
    /// `null` in JSON for a slice with no endpoints.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SliceMeta {
    #[serde(default, deserialize_with = "null_as_empty_map")]
    pub labels: HashMap<String, String>,
}

impl EndpointSlice {
    /// The Service the slice belongs to, from its Service-name label.
    pub fn service(&self) -> Option<&str> {
        self.metadata
            .labels
            .get(SERVICE_NAME_LABEL)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Endpoint {
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub conditions: Conditions,
    #[serde(rename = "targetRef", default)]
    pub target_ref: Option<TargetRef>,
}

/// An endpoint's conditions. Each is optional in the API.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Conditions {
    #[serde(default)]
    pub ready: Option<bool>,
    #[serde(default)]
    pub serving: Option<bool>,
    #[serde(default)]
    pub terminating: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetRef {
    #[serde(default)]
    pub uid: Option<String>,
}

fn null_as_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

fn null_as_empty_map<'de, D>(d: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<HashMap<String, String>>::deserialize(d)?.unwrap_or_default())
}

impl Endpoint {
    /// Counted as a serving replica: `ready` is not false, `serving` is not
    /// false (a missing `serving`, from an older or hand-written slice,
    /// defers to `ready`), and `terminating` is not true (a draining pod can
    /// stay serving through its grace period, but takes no new requests). A
    /// missing `ready` counts as ready, as the EndpointSlice API specifies
    /// for an unknown state.
    pub fn is_serving(&self) -> bool {
        let c = &self.conditions;
        c.ready != Some(false) && c.serving != Some(false) && c.terminating != Some(true)
    }

    /// What identifies the backend behind the endpoint across slices: the
    /// pod's uid, else its addresses.
    fn key(&self) -> Option<String> {
        if let Some(uid) = self
            .target_ref
            .as_ref()
            .and_then(|t| t.uid.as_deref())
            .filter(|u| !u.is_empty())
        {
            return Some(format!("uid:{uid}"));
        }
        let mut addresses: Vec<&str> = self
            .addresses
            .iter()
            .map(String::as_str)
            .filter(|a| !a.is_empty())
            .collect();
        if addresses.is_empty() {
            return None;
        }
        addresses.sort_unstable();
        Some(format!("addr:{}", addresses.join(",")))
    }
}

/// What a Service's slices say: how many distinct endpoints they list, and
/// how many of those serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointCount {
    pub total: usize,
    pub ready: usize,
}

/// Count the distinct endpoints across a Service's slices. The same backend
/// can appear in more than one slice (one per address family, or twice for a
/// moment while the controller moves it between slices), so endpoints are
/// de-duplicated by the pod's uid, else by address, and one counts as ready
/// when any of its copies is. An endpoint with neither is skipped.
pub fn count_endpoints(slices: &[EndpointSlice]) -> EndpointCount {
    let mut all: HashSet<String> = HashSet::new();
    let mut ready: HashSet<String> = HashSet::new();
    for e in slices.iter().flat_map(|s| s.endpoints.iter()) {
        let Some(key) = e.key() else { continue };
        if e.is_serving() {
            ready.insert(key.clone());
        }
        all.insert(key);
    }
    EndpointCount {
        total: all.len(),
        ready: ready.len(),
    }
}

/// Ready endpoints per Service among one namespace's slices, counted like
/// [`count_endpoints`] counts one Service's, by Service name. Slices without
/// the Service-name label are skipped.
pub fn ready_by_service(slices: &[EndpointSlice]) -> BTreeMap<String, usize> {
    let mut by_service: BTreeMap<String, Vec<EndpointSlice>> = BTreeMap::new();
    for slice in slices {
        if let Some(service) = slice.service() {
            by_service
                .entry(service.to_string())
                .or_default()
                .push(slice.clone());
        }
    }
    by_service
        .into_iter()
        .map(|(service, slices)| (service, count_endpoints(&slices).ready))
        .collect()
}
