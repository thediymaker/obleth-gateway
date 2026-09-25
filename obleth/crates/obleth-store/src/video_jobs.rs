//! Video generation jobs: which tenant owns each job id, and where it lives.
//!
//! The OpenAI Videos API hands back a job id at create time, and every later
//! call (poll, download, delete) carries that id alone — no model. The data
//! plane records the id here when a create succeeds and reads it back on each
//! follow-up, so it can route the call to the model that made the job and
//! refuse it, as not found, for any other tenant.
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

    /// The job, if it exists *and* belongs to `tenant_id`. Another tenant's
    /// job reads exactly like a missing one, so a caller cannot probe for ids.
    pub async fn get_video_job(&self, job_id: &str, tenant_id: Uuid) -> Result<Option<VideoJob>> {
        let row = sqlx::query(
            "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
             from video_jobs where job_id = $1 and tenant_id = $2",
        )
        .bind(job_id)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_job).transpose()
    }

    /// The tenant's jobs, newest first, at most `limit`.
    pub async fn list_video_jobs(&self, tenant_id: Uuid, limit: i64) -> Result<Vec<VideoJob>> {
        let rows = sqlx::query(
            "select job_id, model_name, tenant_id, key_id, upstream_base, created_at
             from video_jobs where tenant_id = $1
             order by created_at desc limit $2",
        )
        .bind(tenant_id)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_job).collect()
    }

    /// Forget a tenant's job. Returns whether a row was removed.
    pub async fn delete_video_job(&self, job_id: &str, tenant_id: Uuid) -> Result<bool> {
        let done = sqlx::query("delete from video_jobs where job_id = $1 and tenant_id = $2")
            .bind(job_id)
            .bind(tenant_id)
            .execute(&self.pool)
            .await?;
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

        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let key = Uuid::new_v4();
        let job = format!("video_{}", Uuid::new_v4().simple());
        store
            .insert_video_job(&job, "wan-2-2", owner, key, "http://wan:8000/v1")
            .await
            .expect("insert");

        let found = store
            .get_video_job(&job, owner)
            .await
            .expect("get")
            .expect("the owner sees its job");
        assert_eq!(found.model_name, "wan-2-2");
        assert_eq!(found.key_id, key);
        assert_eq!(found.upstream_base, "http://wan:8000/v1");
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
            .insert_video_job(&job, "wan-2-2", owner, key, "http://wan:8000/v1")
            .await
            .expect("insert");
        assert!(store.delete_video_job(&job, owner).await.expect("delete"));
        assert!(!store.delete_video_job(&job, owner).await.expect("delete"));
    }
}
