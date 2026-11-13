//! Tenant members: who belongs to a tenant and with which role.

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub tenant_id: Uuid,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub kind: String,
    pub role: String,
}

fn member_from_row(r: &PgRow) -> Member {
    Member {
        tenant_id: r.get("tenant_id"),
        issuer: r.get("issuer"),
        subject: r.get("subject"),
        email: r.get("email"),
        role: r.get("role"),
        created_at: r.get("created_at"),
    }
}

impl Store {
    /// Add or update a member. Role must be `manager` or `member`; personal
    /// tenants accept no members through this path.
    pub async fn upsert_member(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
        email: Option<&str>,
        role: &str,
    ) -> Result<Member> {
        if role != "manager" && role != "member" {
            return Err(StoreError::Conflict("invalid role".into()));
        }
        Self::guard_reserved_tenant(tenant_id)?;
        let kind: String = sqlx::query_scalar("select kind from tenants where id = $1")
            .bind(tenant_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        if kind == "personal" {
            return Err(StoreError::Protected(
                "personal tenants have no members".into(),
            ));
        }
        let row = sqlx::query(
            "insert into tenant_members (tenant_id, issuer, subject, email, role) \
             values ($1,$2,$3,$4,$5) \
             on conflict (tenant_id, issuer, subject) do update set \
               role = excluded.role, email = coalesce(excluded.email, tenant_members.email) \
             where tenant_members.role <> 'owner' \
             returning tenant_id, issuer, subject, email, role, created_at",
        )
        .bind(tenant_id)
        .bind(issuer)
        .bind(subject)
        .bind(email)
        .bind(role)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| StoreError::Protected("cannot change the owner".into()))?;
        Ok(member_from_row(&row))
    }

    pub async fn list_members(&self, tenant_id: Uuid) -> Result<Vec<Member>> {
        let rows = sqlx::query(
            "select tenant_id, issuer, subject, email, role, created_at from tenant_members \
             where tenant_id = $1 order by created_at, subject",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(member_from_row).collect())
    }

    pub async fn member_role(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "select role from tenant_members where tenant_id=$1 and issuer=$2 and subject=$3",
        )
        .bind(tenant_id)
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn memberships(&self, issuer: &str, subject: &str) -> Result<Vec<Membership>> {
        let rows = sqlx::query(
            "select t.id, t.name, t.kind, m.role from tenant_members m \
             join tenants t on t.id = m.tenant_id \
             where m.issuer = $1 and m.subject = $2 \
             order by (t.kind = 'personal') desc, t.name",
        )
        .bind(issuer)
        .bind(subject)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Membership {
                tenant_id: r.get("id"),
                tenant_name: r.get("name"),
                kind: r.get("kind"),
                role: r.get("role"),
            })
            .collect())
    }

    /// Remove a member and delete their owned keys in this tenant, in one
    /// transaction. Returns the hashes of the deleted keys (for cache eviction).
    pub async fn remove_member(
        &self,
        tenant_id: Uuid,
        issuer: &str,
        subject: &str,
    ) -> Result<Vec<String>> {
        let mut tx = self.pool.begin().await?;
        let role: String = sqlx::query_scalar(
            "select role from tenant_members where tenant_id=$1 and issuer=$2 and subject=$3 for update",
        )
        .bind(tenant_id)
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;
        if role == "owner" {
            return Err(StoreError::Protected("cannot remove the owner".into()));
        }
        let hashes: Vec<String> = sqlx::query_scalar(
            "delete from api_keys where tenant_id=$1 and owner_issuer=$2 and owner_subject=$3 \
             returning key_hash",
        )
        .bind(tenant_id)
        .bind(issuer)
        .bind(subject)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("delete from tenant_members where tenant_id=$1 and issuer=$2 and subject=$3")
            .bind(tenant_id)
            .bind(issuer)
            .bind(subject)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, FixtureGuard};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn member_lifecycle() {
        let Some(url) = crate::test_support::test_db_url() else {
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        let mut guard = FixtureGuard::new(&store);
        let t = store
            .create_tenant(&format!("p-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        guard.track_tenant(t.id);
        let m = store
            .upsert_member(t.id, "iss", "s1", Some("a@x"), "member")
            .await
            .unwrap();
        assert_eq!(m.role, "member");
        store
            .upsert_member(t.id, "iss", "s1", None, "manager")
            .await
            .unwrap();
        assert_eq!(
            store
                .member_role(t.id, "iss", "s1")
                .await
                .unwrap()
                .as_deref(),
            Some("manager")
        );
        assert_eq!(
            store.list_members(t.id).await.unwrap()[0].email.as_deref(),
            Some("a@x")
        );
        assert!(matches!(
            store.upsert_member(t.id, "iss", "s2", None, "boss").await,
            Err(StoreError::Conflict(_))
        ));
        assert!(store
            .memberships("iss", "s1")
            .await
            .unwrap()
            .iter()
            .any(|x| x.tenant_id == t.id && x.kind == "standard"));
        sqlx::query("insert into api_keys (id, tenant_id, name, key_prefix, key_hash, owner_issuer, owner_subject) values ($1,$2,'k','sk_x',$3,'iss','s1'), ($4,$2,'k2','sk_y',$5,'iss','s9')")
            .bind(Uuid::new_v4()).bind(t.id).bind(format!("h-{}", Uuid::new_v4())).bind(Uuid::new_v4()).bind(format!("h-{}", Uuid::new_v4()))
            .execute(store.pool()).await.unwrap();
        assert_eq!(
            store.remove_member(t.id, "iss", "s1").await.unwrap().len(),
            1
        );
        let left: i64 = sqlx::query_scalar(
            "select count(*) from api_keys where tenant_id=$1 and owner_subject='s9'",
        )
        .bind(t.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(left, 1);
        assert!(store
            .member_role(t.id, "iss", "s1")
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            store.remove_member(t.id, "iss", "s1").await,
            Err(StoreError::NotFound)
        ));
        sqlx::query("update tenants set kind='personal' where id=$1")
            .bind(t.id)
            .execute(store.pool())
            .await
            .unwrap();
        assert!(matches!(
            store.upsert_member(t.id, "iss", "s3", None, "member").await,
            Err(StoreError::Protected(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owner_cannot_be_removed_or_demoted() {
        let Some(url) = crate::test_support::test_db_url() else {
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        let mut guard = FixtureGuard::new(&store);
        let t = store
            .create_tenant(&format!("p-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .unwrap();
        guard.track_tenant(t.id);
        sqlx::query("insert into tenant_members (tenant_id, issuer, subject, role) values ($1,'iss','o','owner')")
            .bind(t.id).execute(store.pool()).await.unwrap();
        assert!(matches!(
            store.remove_member(t.id, "iss", "o").await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.upsert_member(t.id, "iss", "o", None, "member").await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.upsert_member(t.id, "iss", "o", None, "owner").await,
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            store
                .member_role(t.id, "iss", "o")
                .await
                .unwrap()
                .as_deref(),
            Some("owner")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_plane_tenant_refuses_members() {
        let Some(url) = crate::test_support::test_db_url() else {
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        let r = store
            .upsert_member(Store::CONTROL_PLANE_TENANT_ID, "iss", "cp", None, "member")
            .await;
        assert!(matches!(r, Err(StoreError::Protected(_))));
    }
}
