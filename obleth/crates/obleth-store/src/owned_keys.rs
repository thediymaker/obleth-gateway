//! Member-owned keys, member counts and identity lookup.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnedKey {
    #[serde(flatten)]
    pub key: ApiKey,
    pub owner_issuer: String,
    pub owner_subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberCount {
    pub tenant_id: Uuid,
    pub name: String,
    pub members: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityRef {
    pub issuer: String,
    pub subject: String,
    pub email: String,
    pub personal_tenant_id: Option<Uuid>,
}

const OWNED_KEY_COLUMNS: &str = "id, tenant_id, name, description, key_prefix,
    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
    disabled, tracing_enabled, end_user_fairshare, created_at, updated_at,
    kind, identity_issuer, identity_subject, identity_claims,
    weight, max_in_flight, owner_issuer, owner_subject";

fn owned_key_from_row(row: &PgRow) -> Result<OwnedKey> {
    Ok(OwnedKey {
        key: api_key_from_row(row)?,
        owner_issuer: row.try_get("owner_issuer")?,
        owner_subject: row.try_get("owner_subject")?,
    })
}

impl Store {
    /// Member counts of tenants that have members (a personal tenant has its
    /// owner).
    pub async fn member_counts(&self) -> Result<Vec<MemberCount>> {
        let rows = sqlx::query(
            "select t.id, t.name,
                    (select count(*) from tenant_members m where m.tenant_id = t.id) as members
             from tenants t
             where exists (select 1 from tenant_members m where m.tenant_id = t.id)
             order by t.name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MemberCount {
                    tenant_id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    members: r.try_get("members")?,
                })
            })
            .collect()
    }

    /// Create a secret key owned by a tenant member. Membership is checked in
    /// the same statement as the insert; a non-member writes no row.
    pub async fn create_owned_key(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
        name: &str,
    ) -> Result<(OwnedKey, String)> {
        Self::guard_reserved_tenant(tenant_id)?;
        let gen = generate_api_key();
        let sql = format!(
            "insert into api_keys (id, tenant_id, name, description, key_prefix, key_hash,
                    weight, owner_issuer, owner_subject)
             select $1, $2, $3, '', $4, $5, 100, $6, $7
             from (select 1 from tenant_members
                   where tenant_id = $2 and issuer = $6 and subject = $7 for share) m
             returning {OWNED_KEY_COLUMNS}"
        );
        let row = sqlx::query(&sql)
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(name)
            .bind(&gen.prefix)
            .bind(&gen.hash)
            .bind(issuer)
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StoreError::Protected("not a member of this tenant".into()))?;
        Ok((owned_key_from_row(&row)?, gen.secret))
    }

    pub async fn list_owned_keys(
        &self,
        tenant_id: Uuid,
        owner: Option<(&str, &str)>,
    ) -> Result<Vec<OwnedKey>> {
        let sql = format!(
            "select {OWNED_KEY_COLUMNS} from api_keys
             where tenant_id = $1 and owner_subject is not null
               and ($2::text is null or (owner_issuer = $2 and owner_subject = $3))
             order by created_at"
        );
        let rows = sqlx::query(&sql)
            .bind(tenant_id)
            .bind(owner.map(|o| o.0))
            .bind(owner.map(|o| o.1))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(owned_key_from_row).collect()
    }

    /// Known identities (identity keys) whose `email` claim matches, case-insensitively.
    pub async fn identities_by_email(&self, email: &str) -> Result<Vec<IdentityRef>> {
        let rows = sqlx::query(
            "select k.identity_issuer, k.identity_subject,
                    lower(k.identity_claims->>'email') as email,
                    case when t.kind = 'personal' then k.tenant_id end as personal_tenant_id
             from api_keys k join tenants t on t.id = k.tenant_id
             where k.kind = 'identity'
               and lower(k.identity_claims->>'email') = lower($1)
             order by k.identity_issuer, k.identity_subject",
        )
        .bind(email)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(IdentityRef {
                    issuer: r.try_get("identity_issuer")?,
                    subject: r.try_get("identity_subject")?,
                    email: r.try_get("email")?,
                    personal_tenant_id: r.try_get("personal_tenant_id")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owned_keys_and_member_counts() {
        let Some(url) = crate::test_support::test_db_url() else {
            return;
        };
        let _g = crate::test_support::serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        let mut guard = crate::test_support::FixtureGuard::new(&store);
        let t = store
            .create_tenant(&format!("p-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        guard.track_tenant(t.id);
        assert!(matches!(
            store.create_owned_key(t.id, "iss", "s1", "ci").await,
            Err(StoreError::Protected(_))
        ));
        assert!(!store
            .member_counts()
            .await
            .unwrap()
            .iter()
            .any(|p| p.tenant_id == t.id));
        sqlx::query("insert into tenant_members (tenant_id, issuer, subject, role) values ($1,'iss','s1','member')")
            .bind(t.id).execute(store.pool()).await.unwrap();
        let (k, secret) = store
            .create_owned_key(t.id, "iss", "s1", "ci")
            .await
            .unwrap();
        assert!(secret.starts_with("sk_"));
        assert_eq!(k.owner_subject, "s1");
        assert!(store
            .member_counts()
            .await
            .unwrap()
            .iter()
            .any(|p| p.tenant_id == t.id && p.members == 1));
        assert_eq!(
            store
                .list_owned_keys(t.id, Some(("iss", "s1")))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .list_owned_keys(t.id, Some(("iss", "s2")))
            .await
            .unwrap()
            .is_empty());
        let cp = Store::CONTROL_PLANE_TENANT_ID;
        assert!(matches!(
            store.create_owned_key(cp, "iss", "s1", "x").await,
            Err(StoreError::Protected(_))
        ));
        let sub = format!("s-{}", Uuid::new_v4());
        let p = store
            .provision_identity_key(obleth_config::IdentityProvision {
                issuer: "iss",
                subject: &sub,
                tenant_name: &format!("u-{sub}"),
                claims: serde_json::json!({"email": "Bob@X"}),
            })
            .await
            .unwrap();
        guard.track_tenant(p.resolved.tenant_id);
        let ids = store.identities_by_email("bob@x").await.unwrap();
        assert!(ids.iter().any(|i| i.subject == sub));
    }
}
