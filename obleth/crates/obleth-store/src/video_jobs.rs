//! Video generation jobs: which tenant and key own each job id, and where it
//! lives.
//!
//! The OpenAI Videos API hands back a job id at create time, and every later
//! call (poll, download, delete) carries that id alone — no model. The data
//! plane records the id here when a create succeeds and reads it back on each
//! follow-up, so it can route the call to the model that made the job and
//! refuse it, as not found, for anyone who does not own it. Every read and
//! delete takes a [`VideoJobOwner`]: the tenant, and (by default) the key that
//! created the job.
//!
//! This is one of the few data-plane reads of Postgres, and deliberately so:
//! the row must outlive a render that takes minutes, and the chart's Redis
//! evicts keys that carry a TTL under memory pressure. Rows are pruned by age;
//! the backend forgets finished jobs long before that.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::{Result, Store};

/// One recorded video job.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoJob {
    pub job_id: String,
    pub model_name: String,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    /// The base URL that accepted the create; follow-ups go back to it.
    pub upstream_base: String,
    pub created_at: DateTime<Utc>,
}

/// Whose jobs a lookup may see: always one tenant's, and, when `key_id` is
/// set, only the ones that key created. A job outside the filter reads
/// exactly like a missing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoJobOwner {
    pub tenant_id: Uuid,
    /// `Some`: only jobs created by this key. `None`: any key of the tenant.
    pub key_id: Option<Uuid>,
}

impl VideoJobOwner {
    /// Jobs created by `key_id`, within `tenant_id`.
    pub fn key(tenant_id: Uuid, key_id: Uuid) -> Self {
        VideoJobOwner {
            tenant_id,
            key_id: Some(key_id),
        }
    }

    /// Every job of `tenant_id`, whichever of its keys created it.
    pub fn tenant(tenant_id: Uuid) -> Self {
        VideoJobOwner {
            tenant_id,
            key_id: None,
        }
    }
}

fn row_to_job(row: &sqlx::postgres::PgRow) -> Result<VideoJob> {
    Ok(VideoJob {
        job_id: row.try_get("job_id")?,
        model_name: row.try_get("model_name")?,
        tenant_id: row.try_get("tenant_id")?,
        key_id: row.try_get("key_id")?,
        upstream_base: row.try_get("upstream_base")?,
        created_at: row.try_get("created_at")?,
    })
}

impl Store {
    /// Record a job the upstream just created. A repeated id overwrites the
    /// row: ids are minted by the backend, and the latest create is the one
    /// the caller holds.
    pub async fn insert_video_job(
        &self,
        job_id: &str,
        model_name: &str,
        tenant_id: Uuid,
        key_id: Uuid,
        upstream_base: &str,
    ) -> Result<()> {
        sqlx::query(
            "insert into video_jobs (job_id, model_name, tenant_id, key_id, upstream_base)
             values ($1, $2, $3, $4, $5)
             on conflict (job_id) do update set
                model_name = excluded.model_name,
                tenant_id = excluded.tenant_id,
                key_id = excluded.key_id,
                upstream_base = excluded.upstream_base,
                created_at = now()",
        )
        .bind(job_id)
        .bind(model_name)
        .bind(tenant_id)
        .bind(key_id)
        .bind(upstream_base)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The job, if it exists *and* falls within `owner`. A job of another
    /// tenant, or (for a key-scoped owner) of another key, reads exactly like a
    /// missing one, so a caller cannot probe for ids.
    pub async fn get_video_job(
        &self,
        job_id: &str,
        owner: VideoJobOwner,
    ) -> Result<Option<VideoJob>> {
        let query = match owner.key_id {
            Some(key_id) => sqlx::query(
                "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
                 from video_jobs where job_id = $1 and tenant_id = $2 and key_id = $3",
            )
            .bind(job_id)
            .bind(owner.tenant_id)
            .bind(key_id),
            None => sqlx::query(
                "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
                 from video_jobs where job_id = $1 and tenant_id = $2",
            )
            .bind(job_id)
            .bind(owner.tenant_id),
        };
        let row = query.fetch_optional(&self.pool).await?;
        row.as_ref().map(row_to_job).transpose()
    }

    /// The jobs within `owner`, newest first, at most `limit`. Two statements
    /// rather than one with an optional key predicate, so each is planned
    /// against its own index: `(tenant_id, key_id, created_at desc)` for a
    /// key, `(tenant_id, created_at desc)` for a whole tenant.
    pub async fn list_video_jobs(&self, owner: VideoJobOwner, limit: i64) -> Result<Vec<VideoJob>> {
        let query = match owner.key_id {
            Some(key_id) => sqlx::query(
                "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
                 from video_jobs where tenant_id = $1 and key_id = $2
                 order by created_at desc limit $3",
            )
            .bind(owner.tenant_id)
            .bind(key_id),
            None => sqlx::query(
                "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
                 from video_jobs where tenant_id = $1
                 order by created_at desc limit $2",
            )
            .bind(owner.tenant_id),
        };
        let rows = query.bind(limit.max(1)).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_job).collect()
    }

    /// Forget a job within `owner`. Returns whether a row was removed.
    pub async fn delete_video_job(&self, job_id: &str, owner: VideoJobOwner) -> Result<bool> {
        let query = match owner.key_id {
            Some(key_id) => sqlx::query(
                "delete from video_jobs where job_id = $1 and tenant_id = $2 and key_id = $3",
            )
            .bind(job_id)
            .bind(owner.tenant_id)
            .bind(key_id),
            None => sqlx::query("delete from video_jobs where job_id = $1 and tenant_id = $2")
                .bind(job_id)
                .bind(owner.tenant_id),
        };
        let done = query.execute(&self.pool).await?;
        Ok(done.rows_affected() > 0)
    }

    /// Drop every job created before `cutoff`. Returns how many went.
    pub async fn prune_video_jobs(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query("delete from video_jobs where created_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, test_db_url};

    const BASE: &str = "http://video-upstream:8000/v1";

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn video_jobs_are_scoped_to_their_tenant_and_pruned_by_age() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let owner = VideoJobOwner::tenant(Uuid::new_v4());
        let other = VideoJobOwner::tenant(Uuid::new_v4());
        let key = Uuid::new_v4();
        let job = format!("video_{}", Uuid::new_v4().simple());
        store
            .insert_video_job(&job, "video-model", owner.tenant_id, key, BASE)
            .await
            .expect("insert");

        let found = store
            .get_video_job(&job, owner)
            .await
            .expect("get")
            .expect("the owner sees its job");
        assert_eq!(found.model_name, "video-model");
        assert_eq!(found.key_id, key);
        assert_eq!(found.upstream_base, BASE);
        assert!(
            store
                .get_video_job(&job, other)
                .await
                .expect("get")
                .is_none(),
            "another tenant's job must read as missing"
        );

        let listed = store.list_video_jobs(owner, 100).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(store
            .list_video_jobs(other, 100)
            .await
            .expect("list")
            .is_empty());

        // Another tenant cannot delete it either.
        assert!(!store.delete_video_job(&job, other).await.expect("delete"));
        assert!(store
            .get_video_job(&job, owner)
            .await
            .expect("get")
            .is_some());

        // A cutoff before the row keeps it; one after drops it.
        let pruned = store
            .prune_video_jobs(Utc::now() - chrono::Duration::hours(1))
            .await
            .expect("prune");
        assert_eq!(pruned, 0);
        assert!(store
            .get_video_job(&job, owner)
            .await
            .expect("get")
            .is_some());
        store
            .prune_video_jobs(Utc::now() + chrono::Duration::seconds(1))
            .await
            .expect("prune");
        assert!(store
            .get_video_job(&job, owner)
            .await
            .expect("get")
            .is_none());

        // Delete by the owner.
        store
            .insert_video_job(&job, "video-model", owner.tenant_id, key, BASE)
            .await
            .expect("insert");
        assert!(store.delete_video_job(&job, owner).await.expect("delete"));
        assert!(!store.delete_video_job(&job, owner).await.expect("delete"));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_key_scoped_owner_sees_only_the_jobs_its_key_created() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let tenant = Uuid::new_v4();
        let (key_a, key_b) = (Uuid::new_v4(), Uuid::new_v4());
        let a = VideoJobOwner::key(tenant, key_a);
        let b = VideoJobOwner::key(tenant, key_b);
        let whole_tenant = VideoJobOwner::tenant(tenant);
        let outsider = VideoJobOwner::key(Uuid::new_v4(), key_a);
        let job_a = format!("video_{}", Uuid::new_v4().simple());
        let job_b = format!("video_{}", Uuid::new_v4().simple());
        store
            .insert_video_job(&job_a, "video-model", tenant, key_a, BASE)
            .await
            .expect("insert");
        store
            .insert_video_job(&job_b, "video-model", tenant, key_b, BASE)
            .await
            .expect("insert");

        // Each key reads its own job and not the other's.
        assert!(store.get_video_job(&job_a, a).await.unwrap().is_some());
        assert!(store.get_video_job(&job_a, b).await.unwrap().is_none());
        assert!(store.get_video_job(&job_b, a).await.unwrap().is_none());
        // The tenant scope sees both; the same key id under another tenant
        // sees neither.
        assert!(store
            .get_video_job(&job_a, whole_tenant)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_video_job(&job_a, outsider)
            .await
            .unwrap()
            .is_none());

        let ids = |jobs: Vec<VideoJob>| jobs.into_iter().map(|j| j.job_id).collect::<Vec<_>>();
        assert_eq!(
            ids(store.list_video_jobs(a, 100).await.unwrap()),
            vec![job_a.clone()]
        );
        assert_eq!(
            ids(store.list_video_jobs(b, 100).await.unwrap()),
            vec![job_b.clone()]
        );
        // Newest first.
        assert_eq!(
            ids(store.list_video_jobs(whole_tenant, 100).await.unwrap()),
            vec![job_b.clone(), job_a.clone()]
        );
        assert!(store
            .list_video_jobs(outsider, 100)
            .await
            .unwrap()
            .is_empty());

        // B cannot delete A's job; A can.
        assert!(!store.delete_video_job(&job_a, b).await.unwrap());
        assert!(!store.delete_video_job(&job_a, outsider).await.unwrap());
        assert!(store.get_video_job(&job_a, a).await.unwrap().is_some());
        assert!(store.delete_video_job(&job_a, a).await.unwrap());
        // The tenant scope may delete any of its keys' jobs.
        assert!(store.delete_video_job(&job_b, whole_tenant).await.unwrap());
        assert!(store
            .list_video_jobs(whole_tenant, 100)
            .await
            .unwrap()
            .is_empty());
    }
}
