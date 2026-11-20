//! Batches: uploaded files, batch records, and one row per request line.
//!
//! The data plane writes files and batches when a caller creates them, and
//! the batch runner (in every gateway replica) claims items with a lease,
//! finishes them with the upstream's answer, and moves batches through their
//! statuses. Every read on behalf of a caller takes a [`JobOwner`], so another
//! owner's file or batch reads exactly like a missing one.
//!
//! Only state is kept here; the bytes are in the gateway's object store. An
//! uploaded file is a row (its content is chunk objects), an item is its
//! place in the input file (`input_offset`, `input_length`), and an answer is
//! its size (`response_bytes`). Output and error files have no content of
//! their own: their rows point at a batch, and they are read from the batch's
//! items, in line order, when downloaded.
//!
//! Deleting is two-step, objects before rows: the `claim_*` reads hand a
//! replica the files and batches that are due, and the `delete_*_row` calls
//! remove the rows once their objects are gone.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use crate::{JobOwner, Result, Store};

/// Purpose of an uploaded input file.
pub const PURPOSE_BATCH: &str = "batch";
/// Purpose of a batch's output file.
pub const PURPOSE_BATCH_OUTPUT: &str = "batch_output";
/// Purpose of a batch's error file.
pub const PURPOSE_BATCH_ERROR: &str = "batch_error";

/// Statuses a batch is still working in.
const ACTIVE: &str = "('validating', 'in_progress', 'finalizing', 'cancelling')";

/// One file: an uploaded input file, or a batch's output or error file.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchFile {
    pub id: String,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub purpose: String,
    pub filename: String,
    pub bytes: i64,
    /// For output and error files, the batch whose items they are read from.
    pub batch_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A file to record.
#[derive(Debug, Clone)]
pub struct NewBatchFile<'a> {
    pub id: &'a str,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub purpose: &'a str,
    pub filename: &'a str,
    pub bytes: i64,
    pub batch_id: Option<&'a str>,
}

/// A batch being created: the batch row and its items go in one
/// transaction, so a batch is visible only once every item is stored.
pub struct BatchWriter {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    id: String,
    total: i32,
}

impl BatchWriter {
    /// Store a run of items.
    pub async fn add_items(&mut self, items: &[NewBatchItem]) -> Result<()> {
        for chunk in items.chunks(ITEM_CHUNK) {
            let lines: Vec<i32> = chunk.iter().map(|i| i.line).collect();
            let ids: Vec<&str> = chunk.iter().map(|i| i.custom_id.as_str()).collect();
            let offsets: Vec<i64> = chunk.iter().map(|i| i.input_offset).collect();
            let lengths: Vec<i32> = chunk.iter().map(|i| i.input_length).collect();
            sqlx::query(
                "insert into batch_items (batch_id, line, custom_id, input_offset, input_length)
                 select $1, * from unnest($2::int[], $3::text[], $4::bigint[], $5::int[])",
            )
            .bind(&self.id)
            .bind(&lines)
            .bind(&ids)
            .bind(&offsets)
            .bind(&lengths)
            .execute(&mut *self.tx)
            .await?;
            self.total += chunk.len() as i32;
        }
        Ok(())
    }

    /// Commit the batch, `in_progress`, with its item count.
    pub async fn finish(mut self) -> Result<BatchRecord> {
        let row = sqlx::query(&format!(
            "update batches set total = $2 where id = $1 returning {BATCH_COLUMNS}"
        ))
        .bind(&self.id)
        .bind(self.total)
        .fetch_one(&mut *self.tx)
        .await?;
        self.tx.commit().await?;
        row_to_batch(&row)
    }
}

/// One batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRecord {
    pub id: String,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub model: String,
    pub endpoint: String,
    pub input_file_id: String,
    pub output_file_id: Option<String>,
    pub error_file_id: Option<String>,
    pub completion_window: String,
    pub deadline_at: DateTime<Utc>,
    pub status: String,
    pub total: i32,
    pub completed: i32,
    pub failed: i32,
    /// When a request of the batch last finished.
    pub last_progress_at: Option<DateTime<Utc>>,
    /// JSON text.
    pub metadata: Option<String>,
    /// JSON text: the OpenAI `errors` object's `data` list.
    pub errors: Option<String>,
    pub created_at: DateTime<Utc>,
    pub in_progress_at: Option<DateTime<Utc>>,
    pub finalizing_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
    pub expired_at: Option<DateTime<Utc>>,
    pub cancelling_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

/// How long a batch with requests left goes without one finishing before it
/// reads as waiting for capacity.
pub const BATCH_WAITING_AFTER_SECS: i64 = 120;

impl BatchRecord {
    /// Whether the batch is in progress with requests left and has gone
    /// [`BATCH_WAITING_AFTER_SECS`] without one finishing: usually its model is
    /// busy with interactive work, which batches yield to.
    pub fn waiting_for_capacity(&self, now: DateTime<Utc>) -> bool {
        if self.status != "in_progress" || self.completed + self.failed >= self.total {
            return false;
        }
        match self.last_progress_at.or(self.in_progress_at) {
            Some(since) => (now - since).num_seconds() >= BATCH_WAITING_AFTER_SECS,
            None => false,
        }
    }

    /// When the batch reached its final status, if it has.
    pub fn finished_at(&self) -> Option<DateTime<Utc>> {
        self.completed_at
            .or(self.failed_at)
            .or(self.expired_at)
            .or(self.cancelled_at)
    }
}

/// A batch to record.
#[derive(Debug, Clone)]
pub struct NewBatch<'a> {
    pub id: &'a str,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub model: &'a str,
    pub endpoint: &'a str,
    pub input_file_id: &'a str,
    pub completion_window: &'a str,
    pub deadline_at: DateTime<Utc>,
    /// JSON text.
    pub metadata: Option<&'a str>,
}

/// One request line of a batch, as its place in the input file.
#[derive(Debug, Clone, PartialEq)]
pub struct NewBatchItem {
    pub line: i32,
    pub custom_id: String,
    /// Byte offset of the line in the input file.
    pub input_offset: i64,
    /// Length of the line, without its newline.
    pub input_length: i32,
}

/// An item the runner has claimed.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedItem {
    pub batch_id: String,
    pub line: i32,
    pub custom_id: String,
    pub input_offset: i64,
    pub input_length: i32,
    /// Including this one.
    pub attempts: i32,
}

/// One line of an output or error file.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRow {
    pub line: i32,
    pub custom_id: String,
    pub status: String,
    pub response_status: Option<i32>,
    /// Size of the stored answer, when there is one.
    pub response_bytes: Option<i32>,
    /// The attempt that finished the item (its answer's object is named by it).
    pub attempts: i32,
    /// JSON text.
    pub error: Option<String>,
    pub request_id: Option<Uuid>,
}

/// Which file of a batch to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOutput {
    /// Succeeded items.
    Output,
    /// Failed, expired and cancelled items.
    Error,
}

/// Filters for [`Store::list_all_batches`].
#[derive(Debug, Clone, Default)]
pub struct AdminBatchFilter<'a> {
    /// One status, or `active` for every unfinished status.
    pub status: Option<&'a str>,
    pub tenant_id: Option<Uuid>,
    pub model: Option<&'a str>,
}

/// A batch with the names of its tenant and key.
#[derive(Debug, Clone, PartialEq)]
pub struct AdminBatchRow {
    pub batch: BatchRecord,
    pub tenant_name: Option<String>,
    pub key_name: Option<String>,
}

/// A batch that moved to a final status.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishedBatch {
    pub id: String,
    pub status: String,
}

/// An uploaded input file that is due to be deleted (see
/// [`Store::claim_spent_batch_files`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SpentFile {
    pub id: String,
    /// Bytes stored, so far for an abandoned upload.
    pub bytes: i64,
    /// An upload that never finished: one more object may exist past `bytes`.
    pub uploading: bool,
}

/// How long a replica's claim on a file or batch to delete lasts before
/// another may take it over.
const CLEANUP_CLAIM_SECS: f64 = 900.0;

/// Final statuses.
const FINAL: &str = "('completed', 'failed', 'expired', 'cancelled')";

const FILE_COLUMNS: &str = "id, tenant_id, key_id, purpose, filename, bytes, batch_id, created_at";

const BATCH_COLUMNS: &str = "id, tenant_id, key_id, model, endpoint, input_file_id,
    output_file_id, error_file_id, completion_window, deadline_at, status, total, completed,
    failed, last_progress_at, metadata::text as metadata, errors::text as errors, created_at, in_progress_at,
    finalizing_at, completed_at, failed_at, expired_at, cancelling_at, cancelled_at";

fn row_to_file(row: &sqlx::postgres::PgRow) -> Result<BatchFile> {
    Ok(BatchFile {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        key_id: row.try_get("key_id")?,
        purpose: row.try_get("purpose")?,
        filename: row.try_get("filename")?,
        bytes: row.try_get("bytes")?,
        batch_id: row.try_get("batch_id")?,
        created_at: row.try_get("created_at")?,
    })
}

fn row_to_batch(row: &sqlx::postgres::PgRow) -> Result<BatchRecord> {
    Ok(BatchRecord {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        key_id: row.try_get("key_id")?,
        model: row.try_get("model")?,
        endpoint: row.try_get("endpoint")?,
        input_file_id: row.try_get("input_file_id")?,
        output_file_id: row.try_get("output_file_id")?,
        error_file_id: row.try_get("error_file_id")?,
        completion_window: row.try_get("completion_window")?,
        deadline_at: row.try_get("deadline_at")?,
        status: row.try_get("status")?,
        total: row.try_get("total")?,
        completed: row.try_get("completed")?,
        failed: row.try_get("failed")?,
        last_progress_at: row.try_get("last_progress_at")?,
        metadata: row.try_get("metadata")?,
        errors: row.try_get("errors")?,
        created_at: row.try_get("created_at")?,
        in_progress_at: row.try_get("in_progress_at")?,
        finalizing_at: row.try_get("finalizing_at")?,
        completed_at: row.try_get("completed_at")?,
        failed_at: row.try_get("failed_at")?,
        expired_at: row.try_get("expired_at")?,
        cancelling_at: row.try_get("cancelling_at")?,
        cancelled_at: row.try_get("cancelled_at")?,
    })
}

/// `and tenant_id = $n [and key_id = $n+1]` for `owner`, with the binds it
/// needs appended by [`bind_owner`].
fn owner_clause(owner: JobOwner, first: usize) -> String {
    match owner.key_id {
        Some(_) => format!(" and tenant_id = ${first} and key_id = ${}", first + 1),
        None => format!(" and tenant_id = ${first}"),
    }
}

fn bind_owner<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    owner: JobOwner,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    let q = q.bind(owner.tenant_id);
    match owner.key_id {
        Some(key) => q.bind(key),
        None => q,
    }
}

/// Rows per insert when copying a batch's lines into `batch_items`.
const ITEM_CHUNK: usize = 1000;

/// Claims a batch's next pending items in line order. `batch_items_pending_line_idx`
/// returns them in that order, so it reads only the rows it takes.
const CLAIM_ITEMS_SQL: &str = "update batch_items i
     set status = 'running', lease_owner = $2,
         lease_until = now() + make_interval(secs => $4),
         attempts = i.attempts + 1
     where (i.batch_id, i.line) in (
         select batch_id, line from batch_items
         where batch_id = $1 and status = 'pending' and next_attempt_at <= now()
         order by line
         limit $3
         for update skip locked)
     returning i.batch_id, i.line, i.custom_id, i.input_offset, i.input_length,
               i.attempts";
impl Store {
    // ---- files ------------------------------------------------------------

    /// Record an upload that is starting. The file stays hidden from every
    /// read until [`Store::finish_batch_file`]; `file.bytes` is ignored.
    pub async fn begin_batch_file(&self, file: &NewBatchFile<'_>) -> Result<()> {
        sqlx::query(
            "insert into batch_files (id, tenant_id, key_id, purpose, filename, bytes, batch_id,
                                      uploading)
             values ($1, $2, $3, $4, $5, 0, $6, true)",
        )
        .bind(file.id)
        .bind(file.tenant_id)
        .bind(file.key_id)
        .bind(file.purpose)
        .bind(file.filename)
        .bind(file.batch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Note how many bytes of an upload are stored, so that if it is
    /// abandoned its objects can still be found and deleted.
    pub async fn note_batch_file_progress(&self, id: &str, bytes: i64) -> Result<()> {
        sqlx::query("update batch_files set bytes = $2 where id = $1 and uploading")
            .bind(id)
            .bind(bytes)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Take an upload that failed for deletion, as the cleanup would take
    /// it. Returns the bytes it stored, or `None` if it is not an upload.
    pub async fn abandon_batch_file(&self, id: &str) -> Result<Option<i64>> {
        let row = sqlx::query(
            "update batch_files set cleanup_at = now() where id = $1 and uploading returning bytes",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Ok(r.try_get("bytes")?)).transpose()
    }

    /// Finish an upload of `bytes`: the file becomes visible.
    pub async fn finish_batch_file(&self, id: &str, bytes: i64) -> Result<BatchFile> {
        let row = sqlx::query(&format!(
            "update batch_files set bytes = $2, uploading = false
             where id = $1 and uploading returning {FILE_COLUMNS}"
        ))
        .bind(id)
        .bind(bytes)
        .fetch_one(&self.pool)
        .await?;
        row_to_file(&row)
    }

    pub async fn get_batch_file(&self, id: &str, owner: JobOwner) -> Result<Option<BatchFile>> {
        let sql = format!(
            "select {FILE_COLUMNS} from batch_files where id = $1 and not uploading{}",
            owner_clause(owner, 2)
        );
        let row = bind_owner(sqlx::query(&sql).bind(id), owner)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_file).transpose()
    }

    /// Files within `owner`, newest first, after the file `after` (by
    /// creation time), at most `limit`.
    pub async fn list_batch_files(
        &self,
        owner: JobOwner,
        purpose: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BatchFile>> {
        let mut sql = format!(
            "select {FILE_COLUMNS} from batch_files where not uploading{}",
            owner_clause(owner, 1)
        );
        let mut n = if owner.key_id.is_some() { 3 } else { 2 };
        if purpose.is_some() {
            sql.push_str(&format!(" and purpose = ${n}"));
            n += 1;
        }
        if after.is_some() {
            sql.push_str(&format!(
                " and (created_at, id) < (select created_at, id from batch_files where id = ${n})"
            ));
            n += 1;
        }
        sql.push_str(&format!(" order by created_at desc, id desc limit ${n}"));
        let mut q = bind_owner(sqlx::query(&sql), owner);
        if let Some(p) = purpose {
            q = q.bind(p);
        }
        if let Some(a) = after {
            q = q.bind(a);
        }
        let rows = q.bind(limit.clamp(1, 10_000)).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_file).collect()
    }

    /// Whether a batch that is still working reads the input file `id`.
    pub async fn batch_file_in_use(&self, id: &str) -> Result<bool> {
        let row = sqlx::query(&format!(
            "select exists (select 1 from batches
                            where input_file_id = $1 and status in {ACTIVE}) as used"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("used")?)
    }

    /// Delete a file's row once its objects are gone.
    pub async fn delete_batch_file_row(&self, id: &str) -> Result<bool> {
        let done = sqlx::query("delete from batch_files where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    // ---- batches ----------------------------------------------------------

    /// Start a batch that passed validation (see [`BatchWriter`]). It
    /// starts `in_progress` once committed.
    pub async fn begin_batch(&self, batch: &NewBatch<'_>) -> Result<BatchWriter> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "insert into batches (id, tenant_id, key_id, model, endpoint, input_file_id,
                                  completion_window, deadline_at, status, total, metadata,
                                  in_progress_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, 'in_progress', 0, $9::jsonb, now())",
        )
        .bind(batch.id)
        .bind(batch.tenant_id)
        .bind(batch.key_id)
        .bind(batch.model)
        .bind(batch.endpoint)
        .bind(batch.input_file_id)
        .bind(batch.completion_window)
        .bind(batch.deadline_at)
        .bind(batch.metadata)
        .execute(&mut *tx)
        .await?;
        Ok(BatchWriter {
            tx,
            id: batch.id.to_string(),
            total: 0,
        })
    }

    /// Record a batch that passed validation with all of its items at once.
    pub async fn create_batch(
        &self,
        batch: &NewBatch<'_>,
        items: &[NewBatchItem],
    ) -> Result<BatchRecord> {
        let mut w = self.begin_batch(batch).await?;
        w.add_items(items).await?;
        w.finish().await
    }

    /// Record a batch whose input failed validation: status `failed`, with
    /// `errors` (JSON text, a list) saying why.
    pub async fn create_failed_batch(
        &self,
        batch: &NewBatch<'_>,
        errors: &str,
    ) -> Result<BatchRecord> {
        let row = sqlx::query(&format!(
            "insert into batches (id, tenant_id, key_id, model, endpoint, input_file_id,
                                  completion_window, deadline_at, status, metadata, errors,
                                  failed_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, 'failed', $9::jsonb, $10::jsonb, now())
             returning {BATCH_COLUMNS}"
        ))
        .bind(batch.id)
        .bind(batch.tenant_id)
        .bind(batch.key_id)
        .bind(batch.model)
        .bind(batch.endpoint)
        .bind(batch.input_file_id)
        .bind(batch.completion_window)
        .bind(batch.deadline_at)
        .bind(batch.metadata)
        .bind(errors)
        .fetch_one(&self.pool)
        .await?;
        row_to_batch(&row)
    }

    pub async fn get_batch(&self, id: &str, owner: JobOwner) -> Result<Option<BatchRecord>> {
        let sql = format!(
            "select {BATCH_COLUMNS} from batches where id = $1{}",
            owner_clause(owner, 2)
        );
        let row = bind_owner(sqlx::query(&sql).bind(id), owner)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_batch).transpose()
    }

    /// Batches within `owner`, newest first, after the batch `after`, at most
    /// `limit`.
    pub async fn list_batches(
        &self,
        owner: JobOwner,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BatchRecord>> {
        let mut sql = format!(
            "select {BATCH_COLUMNS} from batches where true{}",
            owner_clause(owner, 1)
        );
        let mut n = if owner.key_id.is_some() { 3 } else { 2 };
        if after.is_some() {
            sql.push_str(&format!(
                " and (created_at, id) < (select created_at, id from batches where id = ${n})"
            ));
            n += 1;
        }
        sql.push_str(&format!(" order by created_at desc, id desc limit ${n}"));
        let mut q = bind_owner(sqlx::query(&sql), owner);
        if let Some(a) = after {
            q = q.bind(a);
        }
        let rows = q.bind(limit.clamp(1, 100)).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_batch).collect()
    }

    /// How many batches a key has that are still working. The tenant is
    /// passed so the count reads `batches_owner_idx` (tenant, key, ...).
    pub async fn active_batch_count(&self, tenant_id: Uuid, key_id: Uuid) -> Result<i64> {
        let row = sqlx::query(&format!(
            "select count(*) as n from batches
             where tenant_id = $1 and key_id = $2 and status in {ACTIVE}"
        ))
        .bind(tenant_id)
        .bind(key_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// A batch by id, whoever owns it (the admin API).
    pub async fn get_batch_any(&self, id: &str) -> Result<Option<BatchRecord>> {
        let row = sqlx::query(&format!(
            "select {BATCH_COLUMNS} from batches where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_batch).transpose()
    }

    /// Batches across every tenant, newest first, with their tenant and key
    /// names (the admin API). `status` filters to one status, or to the
    /// unfinished ones with `active`.
    pub async fn list_all_batches(
        &self,
        filter: &AdminBatchFilter<'_>,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AdminBatchRow>> {
        let cols = BATCH_COLUMNS
            .split(',')
            .map(|c| {
                let c = c.trim();
                match c.split_once(" as ") {
                    Some((expr, alias)) => format!("b.{expr} as {alias}"),
                    None => format!("b.{c}"),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut sql = format!(
            "select {cols}, t.name as tenant_name, k.name as key_name
             from batches b
             left join tenants t on t.id = b.tenant_id
             left join api_keys k on k.id = b.key_id
             where true"
        );
        let mut n = 1;
        match filter.status {
            Some("active") => sql.push_str(
                " and b.status in ('validating', 'in_progress', 'finalizing', 'cancelling')",
            ),
            Some(_) => {
                sql.push_str(&format!(" and b.status = ${n}"));
                n += 1;
            }
            None => {}
        }
        if filter.tenant_id.is_some() {
            sql.push_str(&format!(" and b.tenant_id = ${n}"));
            n += 1;
        }
        if filter.model.is_some() {
            sql.push_str(&format!(" and b.model = ${n}"));
            n += 1;
        }
        if after.is_some() {
            sql.push_str(&format!(
                " and (b.created_at, b.id) < (select created_at, id from batches where id = ${n})"
            ));
            n += 1;
        }
        sql.push_str(&format!(
            " order by b.created_at desc, b.id desc limit ${n}"
        ));
        let mut q = sqlx::query(&sql);
        if let Some(s) = filter.status.filter(|s| *s != "active") {
            q = q.bind(s);
        }
        if let Some(t) = filter.tenant_id {
            q = q.bind(t);
        }
        if let Some(m) = filter.model {
            q = q.bind(m);
        }
        if let Some(a) = after {
            q = q.bind(a);
        }
        let rows = q.bind(limit.clamp(1, 200)).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(AdminBatchRow {
                    batch: row_to_batch(r)?,
                    tenant_name: r.try_get("tenant_name")?,
                    key_name: r.try_get("key_name")?,
                })
            })
            .collect()
    }

    /// Batch counts by status across every tenant (the admin API).
    pub async fn batch_status_counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query("select status, count(*) as n from batches group by status")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("status")?, r.try_get("n")?)))
            .collect()
    }

    /// Cancel a batch within `owner`: an active batch moves to `cancelling`
    /// and its pending items to `cancelled`; requests already running finish
    /// and the runner then moves the batch to `cancelled`. A batch in a final
    /// status is returned unchanged. `None` when there is no such batch.
    pub async fn cancel_batch(&self, id: &str, owner: JobOwner) -> Result<Option<BatchRecord>> {
        let mut tx = self.pool.begin().await?;
        let sql = format!(
            "update batches set status = 'cancelling', cancelling_at = now()
             where id = $1 and status in ('validating', 'in_progress'){}
             returning id",
            owner_clause(owner, 2)
        );
        let moved = bind_owner(sqlx::query(&sql).bind(id), owner)
            .fetch_optional(&mut *tx)
            .await?;
        if moved.is_some() {
            sqlx::query(
                "update batch_items set status = 'cancelled', finished_at = now()
                 where batch_id = $1 and status = 'pending'",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_batch(id, owner).await
    }

    /// The answers a batch stored, as (line, attempt) pairs: the names of
    /// its result objects.
    pub async fn batch_result_objects(&self, batch_id: &str) -> Result<Vec<(i32, i32)>> {
        let rows = sqlx::query(
            "select line, attempts from batch_items
             where batch_id = $1 and response_bytes is not null order by line",
        )
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("line")?, r.try_get("attempts")?)))
            .collect()
    }

    /// Delete a batch's rows once its objects are gone: the batch, its items,
    /// and its output and error files' rows. Its input file is not touched.
    pub async fn delete_batch_rows(&self, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query("delete from batches where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("delete from batch_files where batch_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(done.rows_affected() > 0)
    }

    /// Take up to `limit` input files that are due to be deleted, for
    /// `CLEANUP_CLAIM_SECS`: those every batch reading them has finished
    /// with, those no batch read within `retention_secs` of their upload, and
    /// uploads abandoned for `stale_upload_secs`.
    pub async fn claim_spent_batch_files(
        &self,
        retention_secs: i64,
        stale_upload_secs: i64,
        limit: i64,
    ) -> Result<Vec<SpentFile>> {
        let rows = sqlx::query(&format!(
            "update batch_files f set cleanup_at = now()
             where f.id in (
                 select id from batch_files g
                 where g.purpose = 'batch'
                   and (g.cleanup_at is null
                        or g.cleanup_at < now() - make_interval(secs => $4))
                   and case when g.uploading
                            then g.created_at < now() - make_interval(secs => $2)
                            else not exists (select 1 from batches b
                                             where b.input_file_id = g.id
                                               and b.status in {ACTIVE})
                                 and (exists (select 1 from batches b
                                              where b.input_file_id = g.id)
                                      or g.created_at < now() - make_interval(secs => $1))
                       end
                 order by g.created_at
                 limit $3
                 for update skip locked)
             returning f.id, f.bytes, f.uploading"
        ))
        .bind(retention_secs as f64)
        .bind(stale_upload_secs as f64)
        .bind(limit.max(0))
        .bind(CLEANUP_CLAIM_SECS)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(SpentFile {
                    id: r.try_get("id")?,
                    bytes: r.try_get("bytes")?,
                    uploading: r.try_get("uploading")?,
                })
            })
            .collect()
    }

    /// Take up to `limit` batches that have been over for `retention_secs`,
    /// for `CLEANUP_CLAIM_SECS`. Returns their ids.
    pub async fn claim_expired_batches(
        &self,
        retention_secs: i64,
        limit: i64,
    ) -> Result<Vec<String>> {
        let rows = sqlx::query(&format!(
            "update batches b set cleanup_at = now()
             where b.id in (
                 select id from batches c
                 where c.status in {FINAL}
                   and coalesce(c.completed_at, c.failed_at, c.expired_at, c.cancelled_at,
                                c.created_at) < now() - make_interval(secs => $1)
                   and (c.cleanup_at is null
                        or c.cleanup_at < now() - make_interval(secs => $3))
                 order by c.created_at
                 limit $2
                 for update skip locked)
             returning b.id"
        ))
        .bind(retention_secs as f64)
        .bind(limit.max(0))
        .bind(CLEANUP_CLAIM_SECS)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
    }

    // ---- runner -----------------------------------------------------------

    /// Batches with work the runner may start: `in_progress`, before their
    /// deadline, earliest deadline first.
    pub async fn runnable_batches(&self) -> Result<Vec<BatchRecord>> {
        let rows = sqlx::query(&format!(
            "select {BATCH_COLUMNS} from batches
             where status = 'in_progress' and deadline_at > now()
             order by deadline_at, created_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_batch).collect()
    }

    /// Claim up to `limit` pending items of a batch for `instance`, leased
    /// for `lease_secs`. Items another runner holds are skipped, never
    /// waited on.
    pub async fn claim_batch_items(
        &self,
        batch_id: &str,
        instance: &str,
        limit: i64,
        lease_secs: i64,
    ) -> Result<Vec<ClaimedItem>> {
        let rows = sqlx::query(CLAIM_ITEMS_SQL)
            .bind(batch_id)
            .bind(instance)
            .bind(limit.max(0))
            .bind(lease_secs as f64)
            .fetch_all(&self.pool)
            .await?;
        let mut items = rows
            .iter()
            .map(|r| {
                Ok(ClaimedItem {
                    batch_id: r.try_get("batch_id")?,
                    line: r.try_get("line")?,
                    custom_id: r.try_get("custom_id")?,
                    input_offset: r.try_get("input_offset")?,
                    input_length: r.try_get("input_length")?,
                    attempts: r.try_get("attempts")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        items.sort_by_key(|i| i.line);
        Ok(items)
    }

    /// Extend every lease `instance` holds.
    pub async fn renew_batch_leases(&self, instance: &str, lease_secs: i64) -> Result<u64> {
        let done = sqlx::query(
            "update batch_items set lease_until = now() + make_interval(secs => $2)
             where lease_owner = $1 and status = 'running'",
        )
        .bind(instance)
        .bind(lease_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Finish an item `instance` holds with the upstream's successful answer,
    /// already stored (`response_bytes` long) under the item's attempt.
    /// Returns false when the item is no longer this runner's (its lease
    /// lapsed and another took it, or the batch was cancelled).
    ///
    /// Only the item's own row is written. The batch's counts are recomputed
    /// from its items by [`Store::advance_batches`]; updating them here would
    /// make every finishing request wait on the batch row's lock.
    pub async fn complete_batch_item(
        &self,
        batch_id: &str,
        line: i32,
        instance: &str,
        status_code: i32,
        response_bytes: i32,
        request_id: Uuid,
    ) -> Result<bool> {
        let done = sqlx::query(
            "update batch_items
             set status = 'succeeded', response_status = $4, response_bytes = $5,
                 request_id = $6, error = null, lease_owner = null, lease_until = null,
                 finished_at = now()
             where batch_id = $1 and line = $2 and lease_owner = $3 and status = 'running'",
        )
        .bind(batch_id)
        .bind(line)
        .bind(instance)
        .bind(status_code)
        .bind(response_bytes)
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Finish an item `instance` holds as failed, with `error` (JSON text)
    /// and, when the upstream answered, its status and the size of its body
    /// (already stored under the item's attempt).
    #[allow(clippy::too_many_arguments)]
    pub async fn fail_batch_item(
        &self,
        batch_id: &str,
        line: i32,
        instance: &str,
        status_code: Option<i32>,
        response_bytes: Option<i32>,
        error: &str,
        request_id: Option<Uuid>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "update batch_items
             set status = 'failed', response_status = $4, response_bytes = $5,
                 error = $6::jsonb, request_id = $7, lease_owner = null, lease_until = null,
                 finished_at = now()
             where batch_id = $1 and line = $2 and lease_owner = $3 and status = 'running'",
        )
        .bind(batch_id)
        .bind(line)
        .bind(instance)
        .bind(status_code)
        .bind(response_bytes)
        .bind(error)
        .bind(request_id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Put an item `instance` holds back in the queue, to be tried again
    /// after `delay_secs`, noting why (JSON text). With `refund`, the attempt
    /// is not counted (the request never reached an upstream).
    pub async fn retry_batch_item(
        &self,
        batch_id: &str,
        line: i32,
        instance: &str,
        delay_secs: f64,
        error: &str,
        refund: bool,
    ) -> Result<bool> {
        let done = sqlx::query(
            "update batch_items
             set status = 'pending', error = $5::jsonb,
                 attempts = case when $6 then greatest(attempts - 1, 0) else attempts end,
                 next_attempt_at = now() + make_interval(secs => $4),
                 lease_owner = null, lease_until = null
             where batch_id = $1 and line = $2 and lease_owner = $3 and status = 'running'",
        )
        .bind(batch_id)
        .bind(line)
        .bind(instance)
        .bind(delay_secs.max(0.0))
        .bind(error)
        .bind(refund)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Hand back every item `instance` holds, at once and without counting the
    /// attempt: the runner is shutting down. Returns how many.
    pub async fn release_batch_leases(&self, instance: &str) -> Result<u64> {
        let done = sqlx::query(
            "update batch_items
             set status = 'pending', lease_owner = null, lease_until = null,
                 attempts = greatest(attempts - 1, 0), next_attempt_at = now()
             where lease_owner = $1 and status = 'running'",
        )
        .bind(instance)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Return items whose lease lapsed (their runner died or stalled) to the
    /// queue, or fail them once they have had `max_attempts`. Returns how many
    /// were swept.
    pub async fn sweep_batch_leases(&self, max_attempts: i32) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let failed = sqlx::query(
            "update batch_items
             set status = 'failed', lease_owner = null, lease_until = null,
                 finished_at = now(),
                 error = jsonb_build_object('code', 'lease_lapsed',
                     'message', 'the request was retried the most times allowed')
             where status = 'running' and lease_until < now() and attempts >= $1",
        )
        .bind(max_attempts)
        .execute(&mut *tx)
        .await?;
        let requeued = sqlx::query(
            "update batch_items
             set status = 'pending', lease_owner = null, lease_until = null,
                 next_attempt_at = now()
             where status = 'running' and lease_until < now()",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(failed.rows_affected() + requeued.rows_affected())
    }

    /// Recompute the counts of active batches from their items: `completed` is
    /// succeeded items, `failed` is failed and expired ones. Only batches with
    /// an item finished since a minute before their last progress are read: an
    /// item's `finished_at` is its transaction's start, so one that commits
    /// just after a recount can carry a slightly earlier time.
    pub async fn refresh_batch_counts(&self) -> Result<u64> {
        let done = sqlx::query(
            "update batches b set completed = c.ok, failed = c.bad, last_progress_at = c.last
             from (select i.batch_id,
                          count(*) filter (where i.status = 'succeeded')::int as ok,
                          count(*) filter (where i.status in ('failed', 'expired'))::int as bad,
                          max(i.finished_at) filter (where i.status in ('succeeded', 'failed'))
                              as last
                   from batch_items i
                   join batches a on a.id = i.batch_id
                   where a.status in ('in_progress', 'cancelling')
                     and exists (select 1 from batch_items f
                                 where f.batch_id = a.id and f.finished_at is not null
                                   and f.finished_at > coalesce(a.last_progress_at, '-infinity')
                                       - interval '1 minute')
                   group by i.batch_id) c
             where b.id = c.batch_id
               and (b.completed, b.failed, b.last_progress_at)
                   is distinct from (c.ok, c.bad, c.last)",
        )
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Move batches on: expire pending items of batches past their deadline,
    /// refresh the counts of active batches, and give every active batch with
    /// nothing pending or running its final status, with its output and
    /// error files. `new_file_id` mints file ids. Returns the batches that
    /// reached a final status. One replica advances at a time: another that
    /// ticks meanwhile returns at once with nothing.
    pub async fn advance_batches(
        &self,
        new_file_id: &(dyn Fn() -> String + Send + Sync),
    ) -> Result<Vec<FinishedBatch>> {
        // Held until `turn` ends, committed or dropped.
        let mut turn = self.pool.begin().await?;
        let ours: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock($1)")
            .bind(crate::BATCH_ADVANCE_LOCK_KEY)
            .fetch_one(&mut *turn)
            .await?;
        if !ours {
            return Ok(Vec::new());
        }
        // Expire the queue of every batch past its deadline.
        sqlx::query(
            "update batch_items i
             set status = 'expired', finished_at = now(),
                 error = jsonb_build_object('code', 'batch_expired',
                     'message', 'the batch reached its completion window before this request ran')
             from batches b
             where b.id = i.batch_id and b.status = 'in_progress' and b.deadline_at <= now()
               and i.status = 'pending'",
        )
        .execute(&self.pool)
        .await?;
        // A cancelled batch runs nothing more. Cancelling cancels what is
        // pending then, but an item running at that moment can come back to
        // pending later (a retry, or a lapsed lease), and would keep the
        // batch from ever finishing.
        sqlx::query(
            "update batch_items i set status = 'cancelled', finished_at = now()
             from batches b
             where b.id = i.batch_id and b.status = 'cancelling' and i.status = 'pending'",
        )
        .execute(&self.pool)
        .await?;
        self.refresh_batch_counts().await?;

        let candidates = sqlx::query(
            "select b.id from batches b
             where b.status in ('in_progress', 'cancelling')
               and not exists (select 1 from batch_items i
                               where i.batch_id = b.id and i.status in ('pending', 'running'))",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut finished = Vec::new();
        for row in candidates {
            let id: String = row.try_get("id")?;
            if let Some(done) = self.finish_batch(&id, new_file_id).await? {
                finished.push(done);
            }
        }
        turn.commit().await?;
        Ok(finished)
    }

    /// Give one batch its final status, unless another replica already did.
    async fn finish_batch(
        &self,
        id: &str,
        new_file_id: &(dyn Fn() -> String + Send + Sync),
    ) -> Result<Option<FinishedBatch>> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query(
            "select tenant_id, key_id, status, deadline_at <= now() as past_deadline
             from batches where id = $1 and status in ('in_progress', 'cancelling')
             for update",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let busy: bool = sqlx::query(
            "select exists (select 1 from batch_items
                            where batch_id = $1 and status in ('pending', 'running')) as busy",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?
        .try_get("busy")?;
        if busy {
            return Ok(None);
        }
        let tenant_id: Uuid = row.try_get("tenant_id")?;
        let key_id: Uuid = row.try_get("key_id")?;
        let status: String = row.try_get("status")?;
        let past_deadline: bool = row.try_get("past_deadline")?;
        let sizes = sqlx::query(
            "select
                 count(*) filter (where status = 'succeeded') as ok,
                 count(*) filter (where status <> 'succeeded') as bad,
                 count(*) filter (where status in ('failed', 'expired'))::int as failed,
                 coalesce(sum(response_bytes) filter (where status = 'succeeded'), 0)::bigint as ok_bytes,
                 coalesce(sum(coalesce(response_bytes, octet_length(error::text)))
                     filter (where status <> 'succeeded'), 0)::bigint as bad_bytes
             from batch_items where batch_id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let ok: i64 = sizes.try_get("ok")?;
        let bad: i64 = sizes.try_get("bad")?;
        let mut output = None;
        let mut errors = None;
        for (count, bytes_col, purpose, slot) in [
            (ok, "ok_bytes", PURPOSE_BATCH_OUTPUT, &mut output),
            (bad, "bad_bytes", PURPOSE_BATCH_ERROR, &mut errors),
        ] {
            if count == 0 {
                continue;
            }
            let file_id = new_file_id();
            let bytes: i64 = sizes.try_get(bytes_col)?;
            let filename = if purpose == PURPOSE_BATCH_OUTPUT {
                format!("batch_{id}_output.jsonl")
            } else {
                format!("batch_{id}_error.jsonl")
            };
            sqlx::query(
                "insert into batch_files (id, tenant_id, key_id, purpose, filename, bytes, batch_id)
                 values ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&file_id)
            .bind(tenant_id)
            .bind(key_id)
            .bind(purpose)
            .bind(&filename)
            .bind(bytes)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            *slot = Some(file_id);
        }
        let final_status = match (status.as_str(), past_deadline) {
            ("cancelling", _) => "cancelled",
            (_, true) => "expired",
            _ => "completed",
        };
        let stamp = match final_status {
            "cancelled" => "cancelled_at",
            "expired" => "expired_at",
            _ => "completed_at",
        };
        let failed: i32 = sizes.try_get("failed")?;
        sqlx::query(&format!(
            "update batches set status = $2, output_file_id = $3, error_file_id = $4,
                 completed = $5, failed = $6, finalizing_at = now(), {stamp} = now()
             where id = $1"
        ))
        .bind(id)
        .bind(final_status)
        .bind(&output)
        .bind(&errors)
        .bind(ok as i32)
        .bind(failed)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(FinishedBatch {
            id: id.to_string(),
            status: final_status.to_string(),
        }))
    }

    /// A page of a batch's output or error file: items after line
    /// `after_line`, in line order, at most `limit`.
    pub async fn batch_output_rows(
        &self,
        batch_id: &str,
        which: BatchOutput,
        after_line: i32,
        limit: i64,
    ) -> Result<Vec<OutputRow>> {
        let filter = match which {
            BatchOutput::Output => "status = 'succeeded'",
            BatchOutput::Error => "status in ('failed', 'expired', 'cancelled')",
        };
        let rows = sqlx::query(&format!(
            "select line, custom_id, status, response_status, response_bytes, attempts,
                    error::text as error, request_id
             from batch_items
             where batch_id = $1 and line > $2 and {filter}
             order by line limit $3"
        ))
        .bind(batch_id)
        .bind(after_line)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(OutputRow {
                    line: r.try_get("line")?,
                    custom_id: r.try_get("custom_id")?,
                    status: r.try_get("status")?,
                    response_status: r.try_get("response_status")?,
                    response_bytes: r.try_get("response_bytes")?,
                    attempts: r.try_get("attempts")?,
                    error: r.try_get("error")?,
                    request_id: r.try_get("request_id")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, test_db_url};

    fn uid(prefix: &str) -> String {
        format!("{prefix}_{}", Uuid::new_v4().simple())
    }

    /// Connects and migrates. Call it with `serial()` held: `migrate()` re-runs
    /// the schema's DDL, which deadlocks with another test's writes.
    async fn store() -> Option<Store> {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return None;
        };
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        Some(store)
    }

    async fn make_batch(
        store: &Store,
        owner: JobOwner,
        lines: usize,
        deadline: DateTime<Utc>,
    ) -> BatchRecord {
        let id = uid("batch");
        let input = uid("file");
        let items: Vec<NewBatchItem> = (0..lines)
            .map(|i| NewBatchItem {
                line: i as i32,
                custom_id: format!("req-{i}"),
                input_offset: i as i64 * 10,
                input_length: 9,
            })
            .collect();
        store
            .create_batch(
                &NewBatch {
                    id: &id,
                    tenant_id: owner.tenant_id,
                    key_id: owner.key_id.unwrap(),
                    model: "m",
                    endpoint: "/v1/chat/completions",
                    input_file_id: &input,
                    completion_window: "24h",
                    deadline_at: deadline,
                    metadata: Some(r#"{"project":"x"}"#),
                },
                &items,
            )
            .await
            .expect("create")
    }

    /// Record an uploaded file of `bytes`, finished, for `owner`.
    async fn upload(store: &Store, owner: JobOwner, id: &str, bytes: i64) -> BatchFile {
        store
            .begin_batch_file(&NewBatchFile {
                id,
                tenant_id: owner.tenant_id,
                key_id: owner.key_id.unwrap(),
                purpose: PURPOSE_BATCH,
                filename: "in.jsonl",
                bytes: 0,
                batch_id: None,
            })
            .await
            .unwrap();
        store.finish_batch_file(id, bytes).await.unwrap()
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn files_are_scoped_to_their_owner_and_hidden_while_uploading() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let other = JobOwner::key(owner.tenant_id, Uuid::new_v4());
        let id = uid("file");
        store
            .begin_batch_file(&NewBatchFile {
                id: &id,
                tenant_id: owner.tenant_id,
                key_id: owner.key_id.unwrap(),
                purpose: PURPOSE_BATCH,
                filename: "in.jsonl",
                bytes: 0,
                batch_id: None,
            })
            .await
            .unwrap();
        store.note_batch_file_progress(&id, 4096).await.unwrap();
        // Still arriving: nobody sees it.
        assert!(store.get_batch_file(&id, owner).await.unwrap().is_none());
        assert!(store
            .list_batch_files(owner, None, None, 10)
            .await
            .unwrap()
            .is_empty());
        let file = store.finish_batch_file(&id, 5000).await.unwrap();
        assert_eq!(file.bytes, 5000);
        // Finishing twice is refused: the row is no longer an upload.
        assert!(store.finish_batch_file(&id, 1).await.is_err());
        assert!(store.get_batch_file(&id, owner).await.unwrap().is_some());
        assert!(store.get_batch_file(&id, other).await.unwrap().is_none());
        // The tenant scope sees every key's files.
        assert!(store
            .get_batch_file(&id, JobOwner::tenant(owner.tenant_id))
            .await
            .unwrap()
            .is_some());
        upload(&store, owner, &uid("file"), 10).await;
        assert_eq!(
            store
                .list_batch_files(owner, Some(PURPOSE_BATCH), None, 10)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(!store.batch_file_in_use(&id).await.unwrap());
        assert!(store.delete_batch_file_row(&id).await.unwrap());
        assert!(store.get_batch_file(&id, owner).await.unwrap().is_none());
        // A failed upload hands back how far it got, once; a finished file
        // is not an upload.
        let failed = uid("file");
        store
            .begin_batch_file(&NewBatchFile {
                id: &failed,
                tenant_id: owner.tenant_id,
                key_id: owner.key_id.unwrap(),
                purpose: PURPOSE_BATCH,
                filename: "x.jsonl",
                bytes: 0,
                batch_id: None,
            })
            .await
            .unwrap();
        store.note_batch_file_progress(&failed, 123).await.unwrap();
        assert_eq!(store.abandon_batch_file(&failed).await.unwrap(), Some(123));
        let finished = upload(&store, owner, &uid("file"), 1).await;
        assert_eq!(store.abandon_batch_file(&finished.id).await.unwrap(), None);
        store.delete_batch_file_row(&failed).await.unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_claim_reads_pending_items_in_line_order_from_the_index() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(
            &store,
            owner,
            50_000,
            Utc::now() + chrono::Duration::hours(1),
        )
        .await;
        sqlx::query("analyze batch_items")
            .execute(&store.pool)
            .await
            .unwrap();
        let plan: Vec<String> = sqlx::query_scalar(&format!("explain {CLAIM_ITEMS_SQL}"))
            .bind(&batch.id)
            .bind("gw-a")
            .bind(64_i64)
            .bind(60.0_f64)
            .fetch_all(&store.pool)
            .await
            .unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains("batch_items_pending_line_idx"), "{plan}");
        assert!(!plan.contains("Sort"), "{plan}");
        let claimed = store
            .claim_batch_items(&batch.id, "gw-a", 64, 60)
            .await
            .unwrap();
        assert_eq!(claimed.first().map(|i| i.line), Some(0));
        assert_eq!(claimed.len(), 64);
        sqlx::query("delete from batches where id = $1")
            .bind(&batch.id)
            .execute(&store.pool)
            .await
            .unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn one_replica_advances_batches_at_a_time() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 1, Utc::now() + chrono::Duration::hours(1)).await;
        let item = store
            .claim_batch_items(&batch.id, "gw-a", 1, 60)
            .await
            .unwrap();
        assert!(store
            .complete_batch_item(&batch.id, item[0].line, "gw-a", 200, 2, Uuid::new_v4())
            .await
            .unwrap());
        // Another replica is advancing: this one does nothing this tick.
        let mut other = store.pool.acquire().await.unwrap();
        sqlx::query("select pg_advisory_lock($1)")
            .bind(crate::BATCH_ADVANCE_LOCK_KEY)
            .execute(&mut *other)
            .await
            .unwrap();
        assert!(store
            .advance_batches(&|| uid("file"))
            .await
            .unwrap()
            .is_empty());
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!((b.status.as_str(), b.completed), ("in_progress", 0));
        sqlx::query("select pg_advisory_unlock($1)")
            .bind(crate::BATCH_ADVANCE_LOCK_KEY)
            .execute(&mut *other)
            .await
            .unwrap();
        // Its turn: the batch finishes with its count.
        let done = store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(done
            .iter()
            .any(|d| d.id == batch.id && d.status == "completed"));
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!(b.completed, 1);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn items_are_claimed_once_finished_and_the_batch_completes() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 5, Utc::now() + chrono::Duration::hours(1)).await;
        assert_eq!((batch.status.as_str(), batch.total), ("in_progress", 5));
        assert_eq!(
            store
                .active_batch_count(owner.tenant_id, owner.key_id.unwrap())
                .await
                .unwrap(),
            1
        );

        let a = store
            .claim_batch_items(&batch.id, "gw-a", 3, 60)
            .await
            .unwrap();
        let b = store
            .claim_batch_items(&batch.id, "gw-b", 3, 60)
            .await
            .unwrap();
        assert_eq!(a.iter().map(|i| i.line).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(b.iter().map(|i| i.line).collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(a[0].attempts, 1);

        let rid = Uuid::new_v4();
        for item in &a {
            assert!(store
                .complete_batch_item(&batch.id, item.line, "gw-a", 200, 11, rid)
                .await
                .unwrap());
        }
        // Not gw-a's item: refused.
        assert!(!store
            .complete_batch_item(&batch.id, 3, "gw-a", 200, 2, rid)
            .await
            .unwrap());
        assert!(store
            .fail_batch_item(
                &batch.id,
                3,
                "gw-b",
                Some(400),
                Some(3),
                r#"{"code":"bad"}"#,
                None
            )
            .await
            .unwrap());
        // A retry puts it back; the next claim takes it again.
        assert!(store
            .retry_batch_item(&batch.id, 4, "gw-b", 0.0, r#"{"code":"503"}"#, false)
            .await
            .unwrap());
        let again = store
            .claim_batch_items(&batch.id, "gw-a", 10, 60)
            .await
            .unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!((again[0].line, again[0].attempts), (4, 2));
        assert_eq!((again[0].input_offset, again[0].input_length), (40, 9));
        // Not finished while a request is still running (other tests' batches
        // may finish in the same pass).
        assert!(!store
            .advance_batches(&|| uid("file"))
            .await
            .unwrap()
            .iter()
            .any(|d| d.id == batch.id));
        store
            .complete_batch_item(&batch.id, 4, "gw-a", 200, 2, rid)
            .await
            .unwrap();

        let done = store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(done
            .iter()
            .any(|d| d.id == batch.id && d.status == "completed"));
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!(
            (b.status.as_str(), b.completed, b.failed),
            ("completed", 4, 1)
        );
        let out = b.output_file_id.clone().expect("output file");
        let err = b.error_file_id.clone().expect("error file");
        let out_file = store.get_batch_file(&out, owner).await.unwrap().unwrap();
        assert_eq!(out_file.purpose, PURPOSE_BATCH_OUTPUT);
        assert_eq!(out_file.batch_id.as_deref(), Some(batch.id.as_str()));
        assert!(store.get_batch_file(&err, owner).await.unwrap().is_some());
        let rows = store
            .batch_output_rows(&batch.id, BatchOutput::Output, -1, 100)
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![0, 1, 2, 4]
        );
        let page = store
            .batch_output_rows(&batch.id, BatchOutput::Output, 1, 1)
            .await
            .unwrap();
        assert_eq!(page[0].line, 2);
        let errs = store
            .batch_output_rows(&batch.id, BatchOutput::Error, -1, 100)
            .await
            .unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].response_status, Some(400));
        assert_eq!(
            store
                .active_batch_count(owner.tenant_id, owner.key_id.unwrap())
                .await
                .unwrap(),
            0
        );
        // A second pass finds nothing more to do.
        assert!(!store
            .advance_batches(&|| uid("file"))
            .await
            .unwrap()
            .iter()
            .any(|d| d.id == batch.id));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn progress_and_the_admin_list() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 3, Utc::now() + chrono::Duration::hours(1)).await;
        let now = Utc::now();
        assert!(batch.last_progress_at.is_none());
        assert!(!batch.waiting_for_capacity(now));
        // Nothing finished for longer than the waiting threshold.
        let later = now + chrono::Duration::seconds(BATCH_WAITING_AFTER_SECS + 5);
        assert!(batch.waiting_for_capacity(later));

        let got = store.get_batch_any(&batch.id).await.unwrap().unwrap();
        assert_eq!(got.id, batch.id);
        assert!(store.get_batch_any("batch_none").await.unwrap().is_none());

        let active = AdminBatchFilter {
            status: Some("active"),
            tenant_id: Some(owner.tenant_id),
            model: None,
        };
        let rows = store.list_all_batches(&active, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].batch.id, batch.id);
        // No tenant or key rows behind these ids.
        assert_eq!(
            (rows[0].tenant_name.clone(), rows[0].key_name.clone()),
            (None, None)
        );
        let done = AdminBatchFilter {
            status: Some("completed"),
            ..active.clone()
        };
        assert!(store
            .list_all_batches(&done, None, 10)
            .await
            .unwrap()
            .is_empty());
        let other_model = AdminBatchFilter {
            model: Some("other"),
            ..active.clone()
        };
        assert!(store
            .list_all_batches(&other_model, None, 10)
            .await
            .unwrap()
            .is_empty());

        let items = store
            .claim_batch_items(&batch.id, "gw-a", 1, 60)
            .await
            .unwrap();
        store
            .complete_batch_item(&batch.id, items[0].line, "gw-a", 200, 2, Uuid::new_v4())
            .await
            .unwrap();
        store.refresh_batch_counts().await.unwrap();
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!(b.completed, 1);
        let progress = b.last_progress_at.expect("progress recorded");
        assert!(!b.waiting_for_capacity(progress + chrono::Duration::seconds(10)));
        assert!(
            b.waiting_for_capacity(progress + chrono::Duration::seconds(BATCH_WAITING_AFTER_SECS))
        );
        let counts = store.batch_status_counts().await.unwrap();
        assert!(counts.iter().any(|(s, n)| s == "in_progress" && *n >= 1));

        store.cancel_batch(&batch.id, owner).await.unwrap();
        let b = store.get_batch_any(&batch.id).await.unwrap().unwrap();
        assert!(!b.waiting_for_capacity(later));
        // Nothing of it runs: finish it, so no other test's pass picks it up.
        store.advance_batches(&|| uid("file")).await.unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn spent_files_and_expired_batches_are_claimed_for_deletion() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 2, Utc::now() + chrono::Duration::hours(1)).await;
        upload(&store, owner, &batch.input_file_id, 20).await;
        let claimed = |files: Vec<SpentFile>, id: &str| files.iter().any(|f| f.id == id);
        let day = 86_400;

        // Running: its input is in use and nothing of it is due.
        assert!(store.batch_file_in_use(&batch.input_file_id).await.unwrap());
        let spent = store.claim_spent_batch_files(day, day, 1000).await.unwrap();
        assert!(!claimed(spent, &batch.input_file_id));
        assert!(!store
            .claim_expired_batches(0, 1000)
            .await
            .unwrap()
            .contains(&batch.id));

        // Over: its input is due at once, and taken by one claimer only.
        let line = store
            .claim_batch_items(&batch.id, "gw", 1, 60)
            .await
            .unwrap()[0]
            .line;
        store
            .complete_batch_item(&batch.id, line, "gw", 200, 2, Uuid::new_v4())
            .await
            .unwrap();
        store.cancel_batch(&batch.id, owner).await.unwrap();
        store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(!store.batch_file_in_use(&batch.input_file_id).await.unwrap());
        let spent = store.claim_spent_batch_files(day, day, 1000).await.unwrap();
        assert!(claimed(spent, &batch.input_file_id));
        let again = store.claim_spent_batch_files(day, day, 1000).await.unwrap();
        assert!(!claimed(again, &batch.input_file_id));

        // Kept for the retention, then due.
        assert!(!store
            .claim_expired_batches(day, 1000)
            .await
            .unwrap()
            .contains(&batch.id));
        assert!(store
            .claim_expired_batches(0, 1000)
            .await
            .unwrap()
            .contains(&batch.id));
        assert_eq!(
            store.batch_result_objects(&batch.id).await.unwrap(),
            vec![(line, 1)]
        );
        let stopped = store.get_batch_any(&batch.id).await.unwrap().unwrap();
        assert!(store.delete_batch_rows(&batch.id).await.unwrap());
        assert!(store.get_batch_any(&batch.id).await.unwrap().is_none());
        for made in [stopped.output_file_id, stopped.error_file_id]
            .into_iter()
            .flatten()
        {
            assert!(store.get_batch_file(&made, owner).await.unwrap().is_none());
        }
        let items: i64 = sqlx::query_scalar("select count(*) from batch_items where batch_id = $1")
            .bind(&batch.id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(items, 0);
        // The input file is not the batch's to delete.
        assert!(store
            .get_batch_file(&batch.input_file_id, owner)
            .await
            .unwrap()
            .is_some());

        // A file no batch read is due after the retention; an upload
        // abandoned for long enough is due too.
        let unused = upload(&store, owner, &uid("file"), 5).await;
        let stale = uid("file");
        store
            .begin_batch_file(&NewBatchFile {
                id: &stale,
                tenant_id: owner.tenant_id,
                key_id: owner.key_id.unwrap(),
                purpose: PURPOSE_BATCH,
                filename: "x.jsonl",
                bytes: 0,
                batch_id: None,
            })
            .await
            .unwrap();
        store.note_batch_file_progress(&stale, 7).await.unwrap();
        let spent = store.claim_spent_batch_files(day, day, 1000).await.unwrap();
        assert!(!claimed(spent.clone(), &unused.id) && !claimed(spent, &stale));
        sqlx::query(
            "update batch_files set created_at = now() - interval '2 days' where id = any($1)",
        )
        .bind(vec![unused.id.clone(), stale.clone()])
        .execute(&store.pool)
        .await
        .unwrap();
        let spent = store.claim_spent_batch_files(day, day, 1000).await.unwrap();
        assert!(spent.contains(&SpentFile {
            id: unused.id.clone(),
            bytes: 5,
            uploading: false
        }));
        assert!(spent.contains(&SpentFile {
            id: stale.clone(),
            bytes: 7,
            uploading: true
        }));
        for id in [&batch.input_file_id, &unused.id, &stale] {
            assert!(store.delete_batch_file_row(id).await.unwrap());
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn lapsed_leases_return_to_the_queue_or_fail() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 2, Utc::now() + chrono::Duration::hours(1)).await;
        // Leased for no time at all: lapsed at once.
        let claimed = store
            .claim_batch_items(&batch.id, "dead", 2, 0)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 2);
        // Two attempts allowed: line 0 already had one, so it goes back.
        sqlx::query("update batch_items set attempts = 2 where batch_id = $1 and line = 1")
            .bind(&batch.id)
            .execute(&store.pool)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        store.sweep_batch_leases(2).await.unwrap();
        store.refresh_batch_counts().await.unwrap();
        let again = store
            .claim_batch_items(&batch.id, "alive", 5, 60)
            .await
            .unwrap();
        assert_eq!(again.iter().map(|i| i.line).collect::<Vec<_>>(), vec![0]);
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!(b.failed, 1);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_cancelled_batch_ends_when_a_running_item_comes_back_to_pending() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        let batch = make_batch(&store, owner, 2, Utc::now() + chrono::Duration::hours(1)).await;
        let running = store
            .claim_batch_items(&batch.id, "gw", 1, 60)
            .await
            .unwrap();
        store.cancel_batch(&batch.id, owner).await.unwrap().unwrap();
        // The request running at the cancel is retried: back to pending.
        store
            .retry_batch_item(&batch.id, running[0].line, "gw", 0.0, "{}", false)
            .await
            .unwrap();
        let done = store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(done
            .iter()
            .any(|d| d.id == batch.id && d.status == "cancelled"));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn cancel_and_expiry_end_batches() {
        let _g = serial().lock().await;
        let Some(store) = store().await else {
            return;
        };
        let owner = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());

        // Cancel: pending items are cancelled, the running one finishes.
        let batch = make_batch(&store, owner, 3, Utc::now() + chrono::Duration::hours(1)).await;
        let running = store
            .claim_batch_items(&batch.id, "gw", 1, 60)
            .await
            .unwrap();
        let b = store.cancel_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!(b.status, "cancelling");
        assert!(store
            .claim_batch_items(&batch.id, "gw", 5, 60)
            .await
            .unwrap()
            .is_empty());
        assert!(!store
            .advance_batches(&|| uid("file"))
            .await
            .unwrap()
            .iter()
            .any(|d| d.id == batch.id));
        store
            .complete_batch_item(&batch.id, running[0].line, "gw", 200, 2, Uuid::new_v4())
            .await
            .unwrap();
        let done = store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(done
            .iter()
            .any(|d| d.id == batch.id && d.status == "cancelled"));
        let b = store.get_batch(&batch.id, owner).await.unwrap().unwrap();
        assert_eq!((b.completed, b.failed), (1, 0));
        assert!(b.cancelled_at.is_some());
        // Another owner cannot cancel or see it.
        let stranger = JobOwner::key(Uuid::new_v4(), Uuid::new_v4());
        assert!(store
            .cancel_batch(&batch.id, stranger)
            .await
            .unwrap()
            .is_none());

        // Expiry: past the deadline, pending items expire into the error file.
        let late = make_batch(&store, owner, 2, Utc::now() - chrono::Duration::seconds(1)).await;
        assert!(store
            .runnable_batches()
            .await
            .unwrap()
            .iter()
            .all(|b| b.id != late.id));
        let done = store.advance_batches(&|| uid("file")).await.unwrap();
        assert!(done
            .iter()
            .any(|d| d.id == late.id && d.status == "expired"));
        let b = store.get_batch(&late.id, owner).await.unwrap().unwrap();
        assert_eq!(b.failed, 2);
        assert!(b.output_file_id.is_none());
        let errs = store
            .batch_output_rows(&late.id, BatchOutput::Error, -1, 10)
            .await
            .unwrap();
        assert!(errs.iter().all(|r| r.status == "expired"));
        assert!(errs[0].error.as_deref().unwrap().contains("batch_expired"));
    }
}
