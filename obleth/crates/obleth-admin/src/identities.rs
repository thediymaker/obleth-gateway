//! Identity provisioning for the control plane: personal tenant on sign-in,
//! lookup by email, and memberships of an identity.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use obleth_config::{is_personal_template, render_identity_tenant, IdentityProvision};
use obleth_store::{IdentityRef, Membership, ProvisionOptions};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, push_key, AdminError, AdminState, Result};

pub(crate) fn router() -> Router<AdminState> {
    Router::new()
        .route(
            "/api/v1/identities",
            get(identities_by_email).post(provision_identity),
        )
        .route("/api/v1/identities/memberships", get(memberships))
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct ProvisionIdentityBody {
    issuer: String,
    subject: String,
    email: String,
    #[serde(default)]
    rehome: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ProvisionedIdentityOut {
    tenant_id: Uuid,
    tenant_name: String,
    key_id: Uuid,
    tenant_created: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EmailQuery {
    #[serde(default)]
    email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MembershipQuery {
    #[serde(default)]
    issuer: String,
    #[serde(default)]
    subject: String,
}

#[utoipa::path(
    post, path = "/api/v1/identities", tag = "identities",
    request_body = ProvisionIdentityBody,
    responses(
        (status = 200, body = ProvisionedIdentityOut),
        (status = 400, description = "Missing subject or email, or unknown issuer")
    )
)]
pub(crate) async fn provision_identity(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<ProvisionIdentityBody>,
) -> Result<Json<ProvisionedIdentityOut>> {
    let subject = body.subject.trim();
    let email = body.email.trim();
    if subject.is_empty() {
        return Err(AdminError::BadRequest("subject is required".into()));
    }
    if email.is_empty() {
        return Err(AdminError::BadRequest("email is required".into()));
    }
    let cfg = state
        .jwt_issuers
        .iter()
        .find(|c| c.issuer == body.issuer)
        .ok_or_else(|| AdminError::BadRequest("unknown issuer".into()))?;
    let claims = serde_json::json!({ "email": email });
    let tenant_name = render_identity_tenant(&cfg.tenant, subject, &claims)
        .map_err(|e| AdminError::BadRequest(e.to_string()))?;
    let personal = is_personal_template(&cfg.tenant);
    let provisioned = state
        .store
        .provision_identity_key_with(
            IdentityProvision {
                issuer: &cfg.issuer,
                subject,
                tenant_name: &tenant_name,
                claims: claims.clone(),
            },
            ProvisionOptions {
                defaults: cfg.tenant_defaults.as_ref(),
                personal,
                rehome: body.rehome,
            },
        )
        .await?;
    // Audited before the cache push: the rows are committed even if the push fails.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "provision_identity",
            "tenant",
            &provisioned.resolved.tenant_id.to_string(),
            serde_json::json!({
                "issuer": cfg.issuer, "subject": subject, "rehome": body.rehome,
                "tenant_created": provisioned.tenant_created,
                "key_created": provisioned.key_created,
            }),
        )
        .await?;
    push_key(&state, &provisioned.hash, &provisioned.resolved).await?;
    Ok(Json(ProvisionedIdentityOut {
        tenant_id: provisioned.resolved.tenant_id,
        tenant_name: provisioned.resolved.tenant_name.clone(),
        key_id: provisioned.resolved.key_id,
        tenant_created: provisioned.tenant_created,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/identities", tag = "identities",
    params(("email" = String, Query, description = "Email to look up (case-insensitive)")),
    responses((status = 200, description = "Identities known by that email"), (status = 400))
)]
pub(crate) async fn identities_by_email(
    State(state): State<AdminState>,
    Query(q): Query<EmailQuery>,
) -> Result<Json<Vec<IdentityRef>>> {
    let email = q.email.trim();
    if email.is_empty() {
        return Err(AdminError::BadRequest("email is required".into()));
    }
    let mut rows = state.store.identities_by_email(email).await?;
    rows.retain(|r| state.jwt_issuers.iter().any(|c| c.issuer == r.issuer));
    Ok(Json(rows))
}

#[utoipa::path(
    get, path = "/api/v1/identities/memberships", tag = "identities",
    params(
        ("issuer" = String, Query, description = "Identity issuer"),
        ("subject" = String, Query, description = "Identity subject"),
    ),
    responses((status = 200, description = "Tenants the identity belongs to, with role and kind"), (status = 400))
)]
pub(crate) async fn memberships(
    State(state): State<AdminState>,
    Query(q): Query<MembershipQuery>,
) -> Result<Json<Vec<Membership>>> {
    if q.issuer.trim().is_empty() || q.subject.trim().is_empty() {
        return Err(AdminError::BadRequest(
            "issuer and subject are required".into(),
        ));
    }
    Ok(Json(state.store.memberships(&q.issuer, &q.subject).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::harness::serial;
    use crate::*;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const TOKEN: &str = "ident-test-token";

    fn issuer_cfg() -> obleth_config::JwtIssuerConfig {
        obleth_config::parse_jwt_issuers(
            r#"[{"issuer":"https://iss.example","audience":"a","tenant":"u-{sub}","tenant_defaults":{"budget_tokens":1000,"budget_period":"monthly"}}]"#,
        )
        .unwrap()
        .remove(0)
    }

    async fn app() -> Option<(Router, Store, RedisStore)> {
        app_with_issuers(vec![issuer_cfg()]).await
    }

    async fn app_with_issuers(
        issuers: Vec<obleth_config::JwtIssuerConfig>,
    ) -> Option<(Router, Store, RedisStore)> {
        let db_url = std::env::var("OBLETH_TEST_DATABASE_URL").ok()?;
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        let store = Store::connect(&db_url).await.unwrap();
        store.migrate().await.unwrap();
        let redis = RedisStore::connect(&redis_url).await.unwrap();
        let capacity = Arc::new(StaticCapacity::new(64));
        let fairshare = FairShare::start(
            capacity.clone(),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let http = reqwest::Client::new();
        let state = AdminState {
            store: store.clone(),
            redis: redis.clone(),
            capacity,
            fairshare: fairshare.clone(),
            fairshare_stats: fairshare.stats(),
            default_model_max_in_flight: 32,
            fairshare_history: Arc::new(FairshareHistory::new(1800)),
            fairshare_history_secs: 3600,
            fairshare_replica_aware: true,
            fairshare_shared_slots: false,
            clickhouse: clickhouse::Client::default(),
            admin_token: TOKEN.to_string(),
            output_stats: Default::default(),
            classify: None,
            capacity_discovery: capacity_discovery::CapacityDiscovery::disabled(),
            judge: None,
            jwt_issuers: issuers,
            health: ModelHealthRuntime {
                scheduled_enabled: false,
                default_interval_secs: 60,
                timeout_secs: 5,
                retention_days: 1,
                http: http.clone(),
                alerts: None,
                telemetry: None,
                catalogs: Default::default(),
            },
            usage_retention_default_days: 30,
            ssrf: ssrf::SsrfPolicy::from_env(),
            alerts: AlertDispatcher::new(http, AlertSettings::default()),
            local_cache_tx: None,
        };
        Some((crate::router(state), store, redis))
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        auth: bool,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri(path);
        if auth {
            req = req.header("authorization", format!("Bearer {TOKEN}"));
        }
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                axum::body::Body::from(b.to_string())
            }
            None => axum::body::Body::empty(),
        };
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identities_roundtrip() {
        let _g = serial().lock().await;
        let Some((app, store, redis)) = app().await else {
            return;
        };
        let sub = format!("s-{}", Uuid::new_v4());
        let email = format!("{sub}@x");
        let body =
            serde_json::json!({"issuer": "https://iss.example", "subject": sub, "email": email});
        let (st, j) = call(&app, "POST", "/api/v1/identities", Some(body.clone()), true).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        assert_eq!(j["tenant_name"], format!("u-{sub}"));
        assert_eq!(j["tenant_created"], true);
        let tenant_id: Uuid = j["tenant_id"].as_str().unwrap().parse().unwrap();

        // provision_warms_cache: the identity key is resolvable from Redis.
        let hash = obleth_config::identity_key_hash("https://iss.example", &sub);
        let cached = redis.get_resolved_key(&hash).await.unwrap();
        assert_eq!(cached.map(|r| r.tenant_id), Some(tenant_id));

        let (_, j) = call(&app, "POST", "/api/v1/identities", Some(body), true).await;
        assert_eq!(j["tenant_created"], false);

        let (st, j) = call(
            &app,
            "GET",
            &format!("/api/v1/identities/memberships?issuer=https://iss.example&subject={sub}"),
            None,
            true,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(j.as_array().unwrap().len(), 1);
        assert_eq!(j[0]["role"], "owner");
        assert_eq!(j[0]["kind"], "personal");

        let (st, j) = call(
            &app,
            "GET",
            &format!("/api/v1/identities?email={}", email.to_uppercase()),
            None,
            true,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(j
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["subject"] == sub.as_str()));

        let bad = serde_json::json!({"issuer": "nope", "subject": sub, "email": email});
        let (st, _) = call(&app, "POST", "/api/v1/identities", Some(bad), true).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let empty =
            serde_json::json!({"issuer": "https://iss.example", "subject": "", "email": email});
        let (st, _) = call(&app, "POST", "/api/v1/identities", Some(empty), true).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);

        let (st, _) = call(&app, "GET", "/api/v1/identities?email=a@x", None, false).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = call(
            &app,
            "POST",
            "/api/v1/identities",
            Some(serde_json::json!({})),
            false,
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        store.delete_tenant(tenant_id).await.ok();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// TDDEV-5289: a `{email}` tenant template names the personal tenant by
    /// the owner's email address, not the opaque subject.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provision_identity_with_email_template_names_tenant_by_email() {
        let _g = serial().lock().await;
        let mut cfg = issuer_cfg();
        cfg.tenant = "{email}".into();
        let Some((app, store, _redis)) = app_with_issuers(vec![cfg]).await else {
            return;
        };
        let sub = format!("s-{}", Uuid::new_v4());
        let email = format!("{sub}@x");
        let body =
            serde_json::json!({"issuer": "https://iss.example", "subject": sub, "email": email});
        let (st, j) = call(&app, "POST", "/api/v1/identities", Some(body), true).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        // The tenant is named by the owner's email, not the subject.
        assert_eq!(j["tenant_name"], email);
        assert_eq!(j["tenant_created"], true);
        let tenant_id: Uuid = j["tenant_id"].as_str().unwrap().parse().unwrap();

        // The membership is personal and owned by this identity.
        let (st, j) = call(
            &app,
            "GET",
            &format!("/api/v1/identities/memberships?issuer=https://iss.example&subject={sub}"),
            None,
            true,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(j[0]["role"], "owner");
        assert_eq!(j[0]["kind"], "personal");

        store.delete_tenant(tenant_id).await.ok();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identities_by_email_ignores_unconfigured_issuers() {
        let _g = serial().lock().await;
        let Some((app, store, _redis)) = app().await else {
            return;
        };
        let email = format!("stale-{}@x", Uuid::new_v4());
        let mut tenants = Vec::new();
        for (iss, sub) in [
            ("https://iss.example", format!("c-{}", Uuid::new_v4())),
            (
                "http://old.example/realms/x",
                format!("o-{}", Uuid::new_v4()),
            ),
        ] {
            let t = store
                .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 0, None, None)
                .await
                .unwrap();
            store
                .provision_identity_key_with(
                    obleth_config::IdentityProvision {
                        issuer: iss,
                        subject: &sub,
                        tenant_name: &t.name,
                        claims: serde_json::json!({ "email": email }),
                    },
                    obleth_store::ProvisionOptions {
                        defaults: None,
                        personal: false,
                        rehome: false,
                    },
                )
                .await
                .unwrap();
            tenants.push(t.id);
        }
        let (st, j) = call(
            &app,
            "GET",
            &format!("/api/v1/identities?email={email}"),
            None,
            true,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{j}");
        let arr = j.as_array().unwrap();
        assert_eq!(arr.len(), 1, "{j}");
        assert_eq!(arr[0]["issuer"], "https://iss.example");
        for id in tenants {
            store.delete_tenant(id).await.ok();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identities_rehome_and_auth() {
        let _g = serial().lock().await;
        let Some((app, store, redis)) = app().await else {
            return;
        };
        let iss = "https://iss.example";
        for (m, p) in [
            ("GET", "/api/v1/identities?email=a%40x"),
            ("POST", "/api/v1/identities"),
            ("GET", "/api/v1/identities/memberships?issuer=i&subject=s"),
        ] {
            let (st, _) = call(&app, m, p, None, false).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {p}");
        }
        let sub = format!("r-{}", Uuid::new_v4());
        let email = sub.to_string();
        let shared = store
            .create_tenant(&format!("shared-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        let prov = store
            .provision_identity_key_with(
                obleth_config::IdentityProvision {
                    issuer: iss,
                    subject: &sub,
                    tenant_name: &shared.name,
                    claims: serde_json::json!({ "email": email }),
                },
                obleth_store::ProvisionOptions {
                    defaults: None,
                    personal: false,
                    rehome: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(prov.resolved.tenant_id, shared.id);
        let body = |rehome: bool| serde_json::json!({"issuer": iss, "subject": sub, "email": email, "rehome": rehome});
        let (st, j) = call(&app, "POST", "/api/v1/identities", Some(body(false)), true).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        assert_eq!(j["tenant_id"], shared.id.to_string(), "no implicit rehome");
        let (st, j) = call(&app, "POST", "/api/v1/identities", Some(body(true)), true).await;
        assert_eq!(st, StatusCode::OK, "{j}");
        assert_eq!(j["tenant_name"], format!("u-{sub}"));
        let moved: Uuid = j["tenant_id"].as_str().unwrap().parse().unwrap();
        assert_ne!(moved, shared.id);
        let cached = redis.get_resolved_key(&prov.hash).await.unwrap().unwrap();
        assert_eq!(cached.tenant_id, moved);
        // URL issuer in the query is accepted both raw and percent-encoded.
        for q in [
            format!("issuer={iss}&subject={sub}"),
            format!("issuer=https%3A%2F%2Fiss.example&subject={sub}"),
        ] {
            let (st, j) = call(
                &app,
                "GET",
                &format!("/api/v1/identities/memberships?{q}"),
                None,
                true,
            )
            .await;
            assert_eq!(
                (st, j.as_array().map(|a| a.len())),
                (StatusCode::OK, Some(1)),
                "{q}"
            );
        }
        store.delete_tenant(moved).await.ok();
        store.delete_tenant(shared.id).await.ok();
    }
}
