//! Postgres config source-of-truth + audit log.
//!
//! This is the durable, relational backbone behind the Management API: tenants,
//! keys, quotas and a full change history. It is deliberately *off* the request
//! hot path — the data plane reads Redis, which this layer keeps in sync.

use chrono::{DateTime, Utc};
use obleth_config::PgPoolSettings;
use obleth_config::{
    generate_api_key_with_prefix, ApiKey, DeploymentLaunch, FairshareGroup, IdentityProvision,
    ManagedModelSpec, McpServer, ModelEndpoint, ModelHealthCheck, ModelHealthDetail,
    ModelHealthSummary, ModelReplica, ModelRoute, ProvisionedIdentity, ResolvedEndpoint,
    ResolvedKey, ResolvedMcpServer, ResolvedModel, Tenant, WeeklyWindow,
};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::OnceLock;
use uuid::Uuid;

mod audit;
mod backup;
pub mod batches;
mod crypto;
mod endpoints;
mod health;
mod jobs;
mod keys;
pub mod knowledge;
mod managed;
mod mcp;
mod models;
mod models_io;
mod settings;
mod tenants;
pub use audit::*;
pub use backup::BACKUP_KEY_SENTINEL;
pub use crypto::{Cipher, CryptoError};
use endpoints::*;
pub use health::*;
pub use jobs::{JobOwner, JobRecord};
pub use keys::*;
pub use managed::*;
pub use models::*;
pub use models_io::{ModelImportOutcome, ModelImportWrite};
pub use tenants::*;

/// Process-wide cipher for upstream secret columns, initialized once from the
/// environment. Kept global so the row-mapping helpers can transparently
/// decrypt without threading the key through every call site.
static CIPHER: OnceLock<Cipher> = OnceLock::new();

fn cipher() -> &'static Cipher {
    CIPHER.get_or_init(Cipher::from_env)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
    #[error("crypto: {0}")]
    Crypto(#[from] CryptoError),
    #[error("{0}")]
    Conflict(String),
    /// A protected, system-owned resource (e.g. the reserved control-plane
    /// identity) cannot be mutated through the management API.
    #[error("{0}")]
    Protected(String),
}

type Result<T> = std::result::Result<T, StoreError>;

/// One schema file from `schema/postgres/`: its version (the file name
/// without `.sql`) and its SQL, embedded at build time.
macro_rules! migration {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../../../schema/postgres/", $name, ".sql")),
        )
    };
}

/// The schema, in order. `migrate` runs each file a database has not
/// recorded in `schema_migrations`; see `schema/postgres/README.md`.
const MIGRATIONS: &[(&str, &str)] = &[
    migration!("0001_init"),
    migration!("0002_tracing_flag"),
    migration!("0003_guardrails_policy"),
    migration!("0005_managed_launcher_spec"),
    migration!("0006_recipes"),
    migration!("0007_replica_port_and_min_replicas"),
    migration!("0008_managed_provision_error"),
    migration!("0009_replica_cancel_requested"),
    migration!("0010_drop_replica_model_cascade"),
    migration!("0011_endpoint_selection_session_hash"),
    migration!("0012_model_debug_diagnostics"),
    migration!("0013_compression_policy"),
    migration!("0014_model_energy_slots"),
    migration!("0015_tenant_synthetic"),
    migration!("0016_api_keys_identity"),
    migration!("0017_model_route_bias"),
    migration!("0018_knowledge_base"),
    migration!("0019_model_auto_eligible"),
    migration!("0020_knowledge_chunk_unique"),
    migration!("0021_knowledge_reindex_requested"),
    migration!("0022_model_verifier_for"),
    migration!("0023_model_speculation_fields"),
    migration!("0024_model_aliases_and_quantization"),
    migration!("0025_api_key_fairshare"),
    migration!("0026_model_upstream_headers"),
    migration!("0027_model_cost_per_video"),
    migration!("0028_video_jobs"),
    migration!("0029_model_capacity_discovery"),
    migration!("0030_video_jobs_key_index"),
    migration!("0031_deployment_launches"),
    migration!("0032_api_key_end_user_fairshare"),
    migration!("0033_model_variants"),
    migration!("0034_model_lifecycle"),
    migration!("0035_reasoning_effort"),
    migration!("0036_model_rejected_request_fields"),
    migration!("0037_job_kind"),
    migration!("0038_model_session_affinity"),
    migration!("0039_batches"),
    migration!("0040_mcp_server_tenants"),
    migration!("0041_slurm_connector"),
    migration!("0042_api_key_background"),
    migration!("0043_api_key_allowed_networks"),
    migration!("0044_key_prefix_keeps_obl"),
    migration!("0045_api_key_allowed_countries"),
    migration!("0046_batch_object_storage"),
    migration!("0047_batch_item_indexes"),
    migration!("0048_schema_tidy"),
    migration!("0049_knowledge_document_bytes"),
];

/// Arbitrary, fixed key for the advisory lock that serializes `migrate()`
/// across connections, replicas and parallel test binaries.
const MIGRATE_LOCK_KEY: i64 = 0x0B1E_7480_0001;
/// Advisory-lock key serializing reserved control-plane identity provisioning
/// across racing replicas (distinct from `MIGRATE_LOCK_KEY`).
const CONTROL_PLANE_LOCK_KEY: i64 = 0x0B1E_7480_0002;
/// Advisory-lock key letting one replica at a time advance batches.
const BATCH_ADVANCE_LOCK_KEY: i64 = 0x0B1E_7480_0003;

/// Config-side counts for the dashboard overview (see `overview_counts`).
pub struct OverviewCounts {
    pub tenant_count: i64,
    pub key_count: i64,
    pub model_count: i64,
    pub enabled_models: i64,
    /// True when any model has a nonzero per-token price configured.
    pub has_pricing: bool,
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Connect with the default pool settings (tests and tools).
    pub async fn connect(database_url: &str) -> Result<Self> {
        Self::connect_with(database_url, PgPoolSettings::default()).await
    }

    pub async fn connect_with(database_url: &str, pool: PgPoolSettings) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(pool.max_connections)
            .acquire_timeout(pool.acquire_timeout)
            .connect(database_url)
            .await?;
        // Eagerly initialize the cipher so a misconfigured key fails at boot.
        let _ = cipher();
        Ok(Store { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply the embedded schema (CREATE ... IF NOT EXISTS).
    ///
    /// `CREATE TABLE IF NOT EXISTS` is *not* safe under concurrency: two
    /// backends can both pass the existence check and then collide creating the
    /// table's implicit row-type, failing with a duplicate-key violation on
    /// `pg_type_typname_nsp_index`. Since this runs on every gateway boot (and
    /// thus races across replicas / parallel test binaries), we serialize the
    /// whole schema apply behind a session-level advisory lock taken on a single
    /// dedicated connection.
    /// Bring the schema up to date: under an advisory lock (so one replica
    /// applies while the others wait), run every file in [`MIGRATIONS`] that
    /// `schema_migrations` does not record, in order, recording each as it
    /// succeeds. A database with no record yet (new, or made by a gateway
    /// that re-ran every file on each boot) runs every file once, all of
    /// which are re-runnable, and then records the full set.
    pub async fn migrate(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("select pg_advisory_lock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut *conn)
            .await?;
        let result = apply_migrations(&mut conn, MIGRATIONS).await;
        // Always release the lock, even if the schema apply failed.
        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut *conn)
            .await;
        let applied = result?;
        if !applied.is_empty() {
            tracing::info!(files = applied.len(), first = %applied[0], last = %applied[applied.len() - 1], "schema migrations applied");
        }
        Ok(())
    }

    /// The schema files this database has not recorded, without applying
    /// anything (`OBLETH_SCHEMA_VERIFY_ONLY`). Every file, when the database
    /// has no record at all.
    pub async fn pending_migrations(&self) -> Result<Vec<&'static str>> {
        let applied = recorded_migrations(&mut *self.pool.acquire().await?).await?;
        Ok(MIGRATIONS
            .iter()
            .map(|(version, _)| *version)
            .filter(|v| !applied.contains(*v))
            .collect())
    }
}

const SCHEMA_MIGRATIONS_TABLE: &str = "create table if not exists schema_migrations (
    version    text primary key,
    applied_at timestamptz not null default now()
)";

/// The versions `schema_migrations` records; empty when the table does not
/// exist yet.
async fn recorded_migrations(
    conn: &mut sqlx::PgConnection,
) -> Result<std::collections::HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar("select version from schema_migrations")
        .fetch_all(&mut *conn)
        .await
        .or_else(|e| match &e {
            // undefined_table: a database no versioned gateway has migrated.
            sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01") => Ok(Vec::new()),
            _ => Err(e),
        })?;
    Ok(rows.into_iter().collect())
}

/// Apply `files` to the database `conn` is on (the caller holds the
/// migration lock) and return the versions run. See [`Store::migrate`].
async fn apply_migrations(
    conn: &mut sqlx::PgConnection,
    files: &[(&'static str, &'static str)],
) -> Result<Vec<&'static str>> {
    sqlx::raw_sql(SCHEMA_MIGRATIONS_TABLE)
        .execute(&mut *conn)
        .await?;
    let recorded = recorded_migrations(&mut *conn).await?;
    let mut applied = Vec::new();
    if recorded.is_empty() {
        // Baseline: every file once, then the whole set recorded together,
        // so a failure part-way leaves no record and the next boot runs them
        // all again, as before versioning.
        for (version, sql) in files {
            sqlx::raw_sql(sql).execute(&mut *conn).await?;
            applied.push(*version);
        }
        let versions: Vec<&str> = files.iter().map(|(v, _)| *v).collect();
        sqlx::query(
            "insert into schema_migrations (version) select unnest($1::text[])
             on conflict do nothing",
        )
        .bind(&versions)
        .execute(&mut *conn)
        .await?;
        return Ok(applied);
    }
    for (version, sql) in files {
        if recorded.contains(*version) {
            continue;
        }
        // The file and its record commit together, so a file that fails is
        // neither half-applied nor recorded.
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query("insert into schema_migrations (version) values ($1)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        applied.push(*version);
    }
    Ok(applied)
}

/// Truncate to at most `max` chars without splitting a multi-byte character.
/// `str::truncate` panics on a byte index that isn't a char boundary, which a
/// naive `prefix.truncate(64)` risks when the source is an IdP-controlled
/// `sub` claim (not guaranteed ASCII).
fn truncate_chars(s: &str, max: usize) -> String {
    s.char_indices()
        .nth(max)
        .map(|(i, _)| s[..i].to_string())
        .unwrap_or_else(|| s.to_string())
}

/// Shared integration-test plumbing, available to every test module in the
/// crate (`lib.rs` `tests` and `backup.rs` `tests`).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::OnceLock;

    /// Serialises integration tests so DDL from `migrate()` never races with
    /// DML from a concurrently running test, and so two tests that each touch
    /// every row in a table (`export_backup_data`/`restore_backup_data` are
    /// whole-database, not scoped to one test's own fixtures) can't race each
    /// other's snapshots. Shared across every `#[cfg(test)] mod tests` in this
    /// crate (this module, `lib.rs`, and `backup.rs`) rather than one static
    /// per file, since a private static per module does not serialise across
    /// modules -- exactly the gap that let `backup.rs`'s DB tests run
    /// concurrently with `lib.rs`'s despite the latter already holding a lock.
    /// Each DB-backed test must hold this for its full duration, including
    /// the `migrate()` call.
    static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    pub(crate) fn serial() -> &'static tokio::sync::Mutex<()> {
        SERIAL.get_or_init(tokio::sync::Mutex::default)
    }

    /// Reads OBLETH_TEST_DATABASE_URL for an integration test. Returns None when
    /// unset (the test skips). Panics if it IS set but the database name doesn't
    /// contain "test" — refuses to run the integration suite against a possibly
    /// real/dev database, which is how fixtures previously leaked.
    pub(crate) fn test_db_url() -> Option<String> {
        let url = match std::env::var("OBLETH_TEST_DATABASE_URL") {
            Ok(url) => url,
            // Locally a missing database skips the test; in CI it must be
            // there, or the whole integration suite silently stops running.
            Err(_) if std::env::var_os("CI").is_some() => {
                panic!("OBLETH_TEST_DATABASE_URL must be set in CI: the integration tests would otherwise be skipped")
            }
            Err(_) => return None,
        };
        let db = url
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("");
        assert!(
            db.contains("test"),
            "OBLETH_TEST_DATABASE_URL database name {db:?} is not a dedicated test DB \
             (name must contain \"test\", e.g. obleth_test). Refusing to run integration \
             tests against a possibly-real database."
        );
        Some(url)
    }

    /// Tracks the fixture rows a test creates and deletes them when it drops —
    /// which happens even if the test panics on a failed assertion — so leaked
    /// `m-…`/`t-…` rows (and the replica rows they leave behind) never persist
    /// in the target database. Deleting a model cascades to its health
    /// checks/endpoints/managed row, and deleting a tenant cascades to its
    /// keys, so model + tenant ids are enough for those. Replica rows do NOT
    /// cascade from a model delete (the provisioner needs orphan rows to drain
    /// Slurm jobs), so they are tracked and deleted separately.
    ///
    /// Cleanup runs async DB deletes from `Drop`, which requires the test to use
    /// the multi-thread runtime flavor (so `block_in_place` is permitted).
    pub(crate) struct FixtureGuard {
        store: Store,
        models: Vec<Uuid>,
        tenants: Vec<Uuid>,
        replicas: Vec<Uuid>,
        mcp_servers: Vec<Uuid>,
    }

    impl FixtureGuard {
        pub(crate) fn new(store: &Store) -> Self {
            Self {
                store: store.clone(),
                models: Vec::new(),
                tenants: Vec::new(),
                replicas: Vec::new(),
                mcp_servers: Vec::new(),
            }
        }
        pub(crate) fn track_model(&mut self, id: Uuid) {
            self.models.push(id);
        }
        pub(crate) fn track_tenant(&mut self, id: Uuid) {
            self.tenants.push(id);
        }
        pub(crate) fn track_replica(&mut self, id: Uuid) {
            self.replicas.push(id);
        }
        pub(crate) fn track_mcp_server(&mut self, id: Uuid) {
            self.mcp_servers.push(id);
        }
    }

    impl Drop for FixtureGuard {
        fn drop(&mut self) {
            let store = self.store.clone();
            let models = std::mem::take(&mut self.models);
            let tenants = std::mem::take(&mut self.tenants);
            let replicas = std::mem::take(&mut self.replicas);
            let mcp_servers = std::mem::take(&mut self.mcp_servers);
            if models.is_empty()
                && tenants.is_empty()
                && replicas.is_empty()
                && mcp_servers.is_empty()
            {
                return;
            }
            // Deletes are best-effort: a row a test already removed returns
            // NotFound, which we ignore. Replica order vs models is irrelevant
            // since the model_replicas FK was dropped. block_in_place needs the
            // multi-thread flavor on the test.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    for id in models {
                        let _ = store.delete_model(id).await;
                    }
                    for id in tenants {
                        let _ = store.delete_tenant(id).await;
                    }
                    for id in replicas {
                        let _ = store.delete_replica(id).await;
                        // Launch history outlives the replica by design;
                        // fixtures must not leave it behind.
                        let _ = sqlx::query("delete from deployment_launches where id = $1")
                            .bind(id)
                            .execute(&store.pool)
                            .await;
                    }
                    for id in mcp_servers {
                        let _ = store.delete_mcp_server(id).await;
                    }
                });
            });
        }
    }

    // A positional bundle of column values for a model insert; test-only, so the
    // wide tuple is clearer inline than a named alias would be.
    #[allow(clippy::type_complexity)]
    /// The fields an integration-test model is created with before a test
    /// sets the ones it is about.
    pub(crate) fn default_test_fields() -> ModelFields {
        ModelFields {
            description: "integration test model".into(),
            upstream_model: "upstream-model".into(),
            api_base: "http://127.0.0.1:8081".into(),
            model_type: "chat".into(),
            context_window: 8192,
            admission_weight: 100,
            supports_system_messages: true,
            route_bias: 1.0,
            auto_eligible: true,
            capacity_mode: "static".into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, FixtureGuard};

    /// The column names a row mapper reads with `try_get("...")`, including
    /// those read on its behalf by the `*_from_row(row)` helpers it calls.
    fn columns_read_by(mapper_fn: &str) -> std::collections::BTreeSet<&'static str> {
        // The mappers and their helpers live with their entities.
        let src = concat!(include_str!("models.rs"), include_str!("tenants.rs"));
        let body = |name: &str| {
            let at = src
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("no fn {name}"));
            let rest = &src[at..];
            // A free function ends at `}` in column 0; a method at `    }`.
            let end = ["\n}\n", "\n    }\n"]
                .iter()
                .filter_map(|close| rest.find(close))
                .min()
                .unwrap();
            &rest[..end]
        };
        let column = |call: &'static str| {
            let open = call.find('(')? + 1;
            let name = call[open..].trim_start().strip_prefix('"')?;
            Some(&name[..name.find('"')?])
        };
        let mapper = body(mapper_fn);
        let helpers = mapper.match_indices("_from_row(row)").map(|(at, _)| {
            let helper = mapper[..at]
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap();
            body(&format!("{helper}_from_row"))
        });
        std::iter::once(mapper)
            .chain(helpers)
            .flat_map(|text| {
                text.match_indices("try_get")
                    .filter_map(move |(at, _)| column(&text[at..]))
            })
            .collect()
    }

    fn columns_listed(list: &'static str) -> std::collections::BTreeSet<&'static str> {
        list.split(',').map(str::trim).collect()
    }

    /// Every column the model mapper reads is in MODEL_COLUMNS, and nothing in
    /// MODEL_COLUMNS is unread, so the list and the mapper cannot drift apart.
    #[test]
    fn model_columns_match_the_mapper() {
        assert_eq!(
            columns_read_by("model_from_row"),
            columns_listed(super::MODEL_COLUMNS)
        );
    }

    /// Same guarantee for tenants: the mapper and TENANT_COLUMNS agree, so a
    /// returning list can no longer leave tracing_enabled or synthetic out.
    #[test]
    fn tenant_columns_match_the_mapper() {
        assert_eq!(
            columns_read_by("tenant_from_row"),
            columns_listed(super::TENANT_COLUMNS)
        );
    }

    /// No DB needed. `truncate_chars` must never split a multi-byte char —
    /// `p.subject` in `provision_identity_key` is an IdP-controlled `sub`
    /// claim with no guarantee of being ASCII.
    #[test]
    fn truncate_chars_respects_char_boundaries() {
        assert_eq!(truncate_chars("short", 64), "short");
        let exactly_64 = "a".repeat(64);
        assert_eq!(truncate_chars(&exactly_64, 64), exactly_64);
        let long_ascii = "a".repeat(100);
        assert_eq!(truncate_chars(&long_ascii, 64), "a".repeat(64));

        // 70 multi-byte chars ('é' is 2 bytes in UTF-8): truncating at byte
        // index 64 would split the 33rd char and panic; truncating at char
        // index 64 must not.
        let multibyte = "é".repeat(70);
        let truncated = truncate_chars(&multibyte, 64);
        assert_eq!(truncated.chars().count(), 64);
        assert_eq!(truncated, "é".repeat(64));
    }

    /// Tables, columns, indexes and constraints of `public`, one line each,
    /// sorted: two databases with the same text have the same schema.
    async fn schema_fingerprint(pool: &PgPool) -> String {
        sqlx::query_scalar(
            "select string_agg(x, E'\\n' order by x) from (
               select 'col ' || table_name || '.' || column_name || ' ' || data_type || ' '
                      || coalesce(column_default, '') || ' ' || is_nullable
                 from information_schema.columns where table_schema = 'public'
               union all
               select 'idx ' || indexdef from pg_indexes where schemaname = 'public'
               union all
               select 'con ' || conrelid::regclass || ' ' || conname || ' ' || pg_get_constraintdef(oid)
                 from pg_constraint where connamespace = 'public'::regnamespace
             ) s(x)",
        )
        .fetch_one(pool)
        .await
        .expect("fingerprint")
    }

    /// Versioned migrations on a database of their own: a first run applies
    /// and records every file; a second runs none; a database with the
    /// schema but no record (an older gateway's) is brought to the same
    /// state by a baseline run; a new file runs exactly once; a failing file
    /// leaves nothing behind and is not recorded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn migrations_run_once_and_a_baseline_matches_a_fresh_database() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let name = format!("obleth_test_mig_{}", Uuid::new_v4().simple());
        let admin = PgPool::connect(&url).await.expect("connect");
        sqlx::query(&format!("create database {name}"))
            .execute(&admin)
            .await
            .expect("create database");
        let (base, query) = match url.split_once('?') {
            Some((b, q)) => (b, format!("?{q}")),
            None => (url.as_str(), String::new()),
        };
        let own_url = format!("{}/{name}{query}", base.rsplit_once('/').unwrap().0);

        let _drop = DropDatabase {
            admin: admin.clone(),
            name: name.clone(),
        };
        {
            let store = Store::connect(&own_url).await.expect("connect own db");
            store.migrate().await.expect("first migrate");
            assert!(store.pending_migrations().await.unwrap().is_empty());
            let recorded: i64 = sqlx::query_scalar("select count(*) from schema_migrations")
                .fetch_one(&store.pool)
                .await
                .unwrap();
            assert_eq!(recorded as usize, MIGRATIONS.len());
            let fresh = schema_fingerprint(&store.pool).await;
            assert!(
                !fresh.contains("verifier_for"),
                "0048 drops the dead column"
            );
            assert!(!fresh.contains("models_capacity_mode_check"));
            assert!(!fresh.contains("model_replicas_model_idx"));
            assert!(
                fresh.contains("health_check_interval_secs >= 60"),
                "range checks stay"
            );

            let mut conn = store.pool.acquire().await.unwrap();
            let again = apply_migrations(&mut conn, MIGRATIONS).await.unwrap();
            assert!(again.is_empty(), "a second run applies nothing: {again:?}");

            // What a gateway from before versioning leaves: the schema, no record.
            sqlx::query("drop table schema_migrations")
                .execute(&mut *conn)
                .await
                .unwrap();
            assert_eq!(
                store.pending_migrations().await.unwrap().len(),
                MIGRATIONS.len()
            );
            let baseline = apply_migrations(&mut conn, MIGRATIONS).await.unwrap();
            assert_eq!(baseline.len(), MIGRATIONS.len());
            assert_eq!(schema_fingerprint(&store.pool).await, fresh);

            let mut with_new = MIGRATIONS.to_vec();
            with_new.push((
                "9999_probe",
                "create table if not exists migration_probe (n int);
                 insert into migration_probe values (1);",
            ));
            let first = apply_migrations(&mut conn, &with_new).await.unwrap();
            let second = apply_migrations(&mut conn, &with_new).await.unwrap();
            assert_eq!(first, vec!["9999_probe"]);
            assert!(second.is_empty());
            let rows: i64 = sqlx::query_scalar("select count(*) from migration_probe")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
            assert_eq!(rows, 1, "the new file ran once");

            let mut with_bad = with_new.clone();
            with_bad.push((
                "9999_zz_bad",
                "create table bad_probe (n int); select 1 / 0;",
            ));
            assert!(apply_migrations(&mut conn, &with_bad).await.is_err());
            let leftover: Option<String> =
                sqlx::query_scalar("select to_regclass('bad_probe')::text")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
            assert_eq!(leftover, None, "a failed file is rolled back");
            assert_eq!(
                apply_migrations(&mut conn, &with_new).await.unwrap(),
                Vec::<&str>::new()
            );
            let bad_recorded: i64 = sqlx::query_scalar(
                "select count(*) from schema_migrations where version = '9999_zz_bad'",
            )
            .fetch_one(&mut *conn)
            .await
            .unwrap();
            assert_eq!(bad_recorded, 0);
            drop(conn);
            store.pool.close().await;
        }
    }

    /// Drops a test's own database when the test ends, passed or not.
    struct DropDatabase {
        admin: PgPool,
        name: String,
    }

    impl Drop for DropDatabase {
        fn drop(&mut self) {
            let sql = format!("drop database if exists {} with (force)", self.name);
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let _ = sqlx::query(&sql).execute(&self.admin).await;
                })
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn name_reads_match_the_full_rows_and_skip_unknown_ids() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);
        let tenant = store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);
        let (key, _) = store
            .create_api_key(
                tenant.id,
                "named",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create key");
        let stranger = Uuid::new_v4();

        let tenants = store.tenant_names(&[tenant.id, stranger]).await.unwrap();
        assert_eq!(
            tenants,
            vec![TenantName {
                id: tenant.id,
                name: tenant.name.clone(),
                fairshare_group: tenant.fairshare_group.clone(),
            }]
        );
        let keys = store.key_names(&[key.id, stranger]).await.unwrap();
        assert_eq!(
            keys,
            vec![KeyName {
                id: key.id,
                name: "named".into(),
                key_prefix: key.key_prefix.clone(),
            }]
        );
        assert!(store.tenant_names(&[]).await.unwrap().is_empty());
        assert!(store.key_names(&[]).await.unwrap().is_empty());
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn overview_counts_track_entities() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let before = store.overview_counts().await.expect("counts before");

        let tenant = store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);
        store
            .create_api_key(
                tenant.id,
                "k",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create key");

        let after = store.overview_counts().await.expect("counts after");
        assert_eq!(after.tenant_count, before.tenant_count + 1);
        assert_eq!(after.key_count, before.key_count + 1);
        assert_eq!(after.model_count, before.model_count);
        assert_eq!(after.enabled_models, before.enabled_models);
    }
}
