//! Tenant members, owned keys and member counts.
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use obleth_config::hash_api_key;
use obleth_store::{Member, MemberCount, OwnedKey, Store, StoreError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, evict_keys, push_key, AdminError, AdminState, Result};

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route(
            "/api/v1/tenants/:id/members",
            get(list_members).put(put_member).delete(delete_member),
        )
        .route(
            "/api/v1/tenants/:id/owned-keys",
            get(list_owned_keys).post(create_owned_key),
        )
        .route("/api/v1/member-counts", get(member_counts))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PutMember {
    issuer: String,
    subject: String,
    #[serde(default)]
    email: Option<String>,
    /// `member` or `manager`.
    role: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IdentityQuery {
    issuer: Option<String>,
    subject: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateOwnedKey {
    issuer: String,
    subject: String,
    name: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreatedOwnedKey {
    key: OwnedKey,
    secret: String,
}

/// The reserved control-plane tenant is system-owned: no member or key writes.
fn guard_tenant(id: Uuid) -> Result<()> {
    if id == Store::CONTROL_PLANE_TENANT_ID {
        return Err(AdminError::Store(StoreError::Protected(
            "the reserved control-plane tenant cannot be modified".into(),
        )));
    }
    Ok(())
}

fn require_identity(issuer: &str, subject: &str) -> Result<()> {
    if issuer.trim().is_empty() || subject.trim().is_empty() {
        return Err(AdminError::BadRequest(
            "issuer and subject are required".into(),
        ));
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/api/v1/tenants/{id}/members", tag = "members",
    params(("id" = Uuid, Path, description = "Tenant id")),
    responses((status = 200, description = "Members of the tenant"))
)]
pub(crate) async fn list_members(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Member>>> {
    Ok(Json(state.store.list_members(id).await?))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/members", tag = "members",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = PutMember,
    responses((status = 200, description = "The member"), (status = 400), (status = 403))
)]
pub(crate) async fn put_member(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PutMember>,
) -> Result<Json<Member>> {
    guard_tenant(id)?;
    require_identity(&body.issuer, &body.subject)?;
    if body.role != "member" && body.role != "manager" {
        return Err(AdminError::BadRequest(
            "role must be member or manager".into(),
        ));
    }
    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let member = state
        .store
        .upsert_member(id, &body.issuer, &body.subject, email, &body.role)
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "upsert_member",
            "tenant",
            &id.to_string(),
            serde_json::json!({
                "issuer": body.issuer, "subject": body.subject, "role": body.role,
            }),
        )
        .await?;
    Ok(Json(member))
}

#[utoipa::path(
    delete, path = "/api/v1/tenants/{id}/members", tag = "members",
    params(
        ("id" = Uuid, Path, description = "Tenant id"),
        ("issuer" = String, Query, description = "Member issuer"),
        ("subject" = String, Query, description = "Member subject"),
    ),
    responses((status = 204), (status = 403), (status = 404), (status = 502))
)]
pub(crate) async fn delete_member(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Query(q): Query<IdentityQuery>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    guard_tenant(id)?;
    let (issuer, subject) = match (q.issuer, q.subject) {
        (Some(i), Some(s)) => (i, s),
        _ => {
            return Err(AdminError::BadRequest(
                "issuer and subject are required".into(),
            ))
        }
    };
    require_identity(&issuer, &subject)?;
    let hashes = state.store.remove_member(id, &issuer, &subject).await?;
    let evicted = evict_keys(&state, &hashes, "the member's keys").await;
    // Audited before an eviction failure is reported: the rows are gone.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "remove_member",
            "tenant",
            &id.to_string(),
            serde_json::json!({
                "issuer": issuer, "subject": subject,
                "keys_deleted": hashes.len(), "cache_evicted": evicted.is_ok(),
            }),
        )
        .await?;
    evicted?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/api/v1/tenants/{id}/owned-keys", tag = "members",
    params(
        ("id" = Uuid, Path, description = "Tenant id"),
        ("issuer" = Option<String>, Query, description = "Filter by owner issuer"),
        ("subject" = Option<String>, Query, description = "Filter by owner subject"),
    ),
    responses((status = 200, description = "Owned keys"), (status = 400))
)]
pub(crate) async fn list_owned_keys(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Query(q): Query<IdentityQuery>,
) -> Result<Json<Vec<OwnedKey>>> {
    let owner = match (q.issuer.as_deref(), q.subject.as_deref()) {
        (Some(i), Some(s)) => Some((i, s)),
        (None, None) => None,
        _ => {
            return Err(AdminError::BadRequest(
                "issuer and subject go together".into(),
            ))
        }
    };
    Ok(Json(state.store.list_owned_keys(id, owner).await?))
}

#[utoipa::path(
    post, path = "/api/v1/tenants/{id}/owned-keys", tag = "members",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = CreateOwnedKey,
    responses((status = 200, description = "The key and its one-time secret"), (status = 400), (status = 403, description = "Not a member"))
)]
pub(crate) async fn create_owned_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateOwnedKey>,
) -> Result<Json<CreatedOwnedKey>> {
    guard_tenant(id)?;
    require_identity(&body.issuer, &body.subject)?;
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("key name is required".into()));
    }
    let (key, secret) = state
        .store
        .create_owned_key(id, &body.issuer, &body.subject, &name)
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "create_owned_key",
            "api_key",
            &key.key.id.to_string(),
            serde_json::json!({
                "tenant_id": id, "prefix": key.key.key_prefix,
                "issuer": body.issuer, "subject": body.subject,
            }),
        )
        .await?;
    // Audited before the cache push: the row and secret exist even if the push fails.
    let hash = hash_api_key(&secret);
    if let Some(resolved) = state.store.resolved_key_by_hash(&hash).await? {
        push_key(&state, &hash, &resolved).await?;
    }
    Ok(Json(CreatedOwnedKey { key, secret }))
}

#[utoipa::path(
    get, path = "/api/v1/member-counts", tag = "members",
    responses((status = 200, description = "Member counts of tenants that have members"))
)]
pub(crate) async fn member_counts(
    State(state): State<AdminState>,
) -> Result<Json<Vec<MemberCount>>> {
    Ok(Json(state.store.member_counts().await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tower::ServiceExt;

    const TOKEN: &str = "test-admin-token";

    async fn app() -> Option<(Router, Store, tokio::sync::MutexGuard<'static, ()>)> {
        let db_url = std::env::var("OBLETH_TEST_DATABASE_URL").ok()?;
        assert!(db_url.rsplit('/').next().unwrap_or("").contains("test"));
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        let guard = crate::tests::harness::serial().lock().await;
        let store = Store::connect(&db_url).await.expect("connect postgres");
        store.migrate().await.expect("migrate");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let capacity = Arc::new(obleth_fairshare::StaticCapacity::new(64));
        let fairshare = obleth_fairshare::FairShare::start(
            capacity.clone(),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let http = reqwest::Client::new();
        let alerts = crate::AlertDispatcher::new(http.clone(), Default::default());
        let state = AdminState {
            store: store.clone(),
            redis,
            capacity,
            fairshare: fairshare.clone(),
            fairshare_stats: fairshare.stats(),
            default_model_max_in_flight: 32,
            fairshare_history: Arc::new(obleth_fairshare::FairshareHistory::new(1800)),
            fairshare_history_secs: 3600,
            fairshare_replica_aware: true,
            fairshare_shared_slots: false,
            clickhouse: clickhouse::Client::default(),
            admin_token: TOKEN.to_string(),
            output_stats: Default::default(),
            classify: None,
            capacity_discovery: crate::capacity_discovery::CapacityDiscovery::disabled(),
            judge: None,
            jwt_issuers: vec![],
            health: crate::ModelHealthRuntime {
                scheduled_enabled: false,
                default_interval_secs: 60,
                timeout_secs: 5,
                retention_days: 1,
                http,
                alerts: None,
                telemetry: None,
                catalogs: Default::default(),
            },
            usage_retention_default_days: 30,
            ssrf: crate::ssrf::SsrfPolicy::from_env(),
            alerts,
            local_cache_tx: None,
        };
        Some((crate::router(state), store, guard))
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        auth: bool,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = axum::http::Request::builder().method(method).uri(uri);
        if auth {
            req = req.header("authorization", format!("Bearer {TOKEN}"));
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(axum::body::Body::from(b.to_string())),
            None => req.body(axum::body::Body::empty()),
        }
        .expect("build request");
        let res = app.clone().oneshot(req).await.expect("handler ran");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn members_routes_roundtrip() {
        let Some((app, store, _g)) = app().await else {
            return;
        };
        let t = store
            .create_tenant(&format!("p-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        let base = format!("/api/v1/tenants/{}", t.id);
        let m = serde_json::json!({"issuer":"iss","subject":"s1","email":"a@x","role":"member"});

        let (s, b) = send(
            &app,
            "PUT",
            &format!("{base}/members"),
            Some(m.clone()),
            true,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let owner = serde_json::json!({"issuer":"iss","subject":"s1","role":"owner"});
        let (s, _) = send(&app, "PUT", &format!("{base}/members"), Some(owner), true).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, b) = send(&app, "GET", &format!("{base}/members"), None, true).await;
        assert_eq!(
            (s, b.as_array().map(|a| a.len())),
            (StatusCode::OK, Some(1))
        );

        let mk = |sub: &str| serde_json::json!({"issuer":"iss","subject":sub,"name":"ci"});
        let (s, b) = send(
            &app,
            "POST",
            &format!("{base}/owned-keys"),
            Some(mk("s1")),
            true,
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let secret = b["secret"].as_str().unwrap().to_string();
        assert!(secret.starts_with("sk_"));
        assert_eq!(b["key"]["owner_subject"], "s1");
        // owned_key_pushed_to_cache: the resolved key is already in Redis.
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").unwrap();
        let redis = obleth_redis::RedisStore::connect(&redis_url).await.unwrap();
        let hash = hash_api_key(&secret);
        assert!(redis.get_resolved_key(&hash).await.unwrap().is_some());
        let (s, _) = send(
            &app,
            "POST",
            &format!("{base}/owned-keys"),
            Some(mk("s2")),
            true,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        let (s, b) = send(&app, "GET", "/api/v1/member-counts", None, true).await;
        assert_eq!(s, StatusCode::OK);
        assert!(b
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["tenant_id"] == t.id.to_string() && p["members"] == 1));

        let (s, _) = send(
            &app,
            "DELETE",
            &format!("{base}/members?issuer=iss&subject=s1"),
            None,
            true,
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        // delete_member_evicts_keys
        assert!(redis.get_resolved_key(&hash).await.unwrap().is_none());
        let (s, b) = send(
            &app,
            "GET",
            &format!("{base}/owned-keys?issuer=iss&subject=s1"),
            None,
            true,
        )
        .await;
        assert_eq!((s, b), (StatusCode::OK, serde_json::json!([])));

        // reserved control-plane tenant: 403 on every write
        let cp = format!("/api/v1/tenants/{}", Store::CONTROL_PLANE_TENANT_ID);
        for (method, uri, body) in [
            ("PUT", format!("{cp}/members"), Some(m.clone())),
            (
                "DELETE",
                format!("{cp}/members?issuer=iss&subject=s1"),
                None,
            ),
            ("POST", format!("{cp}/owned-keys"), Some(mk("s1"))),
        ] {
            let (s, _) = send(&app, method, &uri, body, true).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}");
        }
        let _ = store.delete_tenant(t.id).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_member_issuer_url_query() {
        let Some((app, store, _g)) = app().await else {
            return;
        };
        let t = store
            .create_tenant(&format!("p-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        let base = format!("/api/v1/tenants/{}/members", t.id);
        let iss = "https://idp.example.com/realms/a+b";
        let m = serde_json::json!({"issuer":iss,"subject":"s 1","role":"manager"});
        let (s, b) = send(&app, "PUT", &base, Some(m), true).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let (s, _) = send(
            &app,
            "DELETE",
            &format!("{base}?issuer=https%3A%2F%2Fidp.example.com%2Frealms%2Fa%2Bb"),
            None,
            true,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let q = "issuer=https%3A%2F%2Fidp.example.com%2Frealms%2Fa%2Bb&subject=s%201";
        let (s, _) = send(&app, "DELETE", &format!("{base}?{q}"), None, true).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = send(&app, "DELETE", &format!("{base}?{q}"), None, true).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let _ = store.delete_tenant(t.id).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_token_401() {
        let Some((app, _store, _g)) = app().await else {
            return;
        };
        let id = Uuid::new_v4();
        for (method, uri) in [
            ("GET", "/api/v1/member-counts".to_string()),
            ("GET", format!("/api/v1/tenants/{id}/members")),
            ("PUT", format!("/api/v1/tenants/{id}/members")),
            (
                "DELETE",
                format!("/api/v1/tenants/{id}/members?issuer=i&subject=s"),
            ),
            ("GET", format!("/api/v1/tenants/{id}/owned-keys")),
            ("POST", format!("/api/v1/tenants/{id}/owned-keys")),
        ] {
            let (s, _) = send(&app, method, &uri, None, false).await;
            assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }
}
