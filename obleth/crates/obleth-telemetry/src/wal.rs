//! Bounded spill segments and checkpointed, at-least-once replay. The original
//! single WAL is read first for upgrade compatibility. One flusher owns this WAL.
//!
//! Each `append` is one insert batch followed by a blank line. Replay stops at
//! that boundary so a replayed batch has exactly the rows (and therefore the
//! ClickHouse deduplication token) of the insert that originally failed.

use obleth_config::UsageRecord;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const MAX_DISK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1024;
const REPLAY_BYTES: usize = 1024 * 1024;
const SEGMENT_TARGET_BYTES: u64 = 1024 * 1024;
const SEGMENT_MAX_AGE: Duration = Duration::from_secs(60);
/// `f64` fields a non-finite value serialised into as JSON `null`.
const FLOAT_FIELDS: [&str; 4] = ["cost_usd", "energy_wh", "energy_cost_usd", "co2_g"];

pub(crate) struct Wal {
    legacy: PathBuf,
    directory: PathBuf,
    max_bytes: u64,
    /// Segment this process is appending to. Never a segment from a previous
    /// process: its tail may be torn, and appending after a torn line would
    /// turn it into a corrupt middle line.
    active: Mutex<Option<Active>>,
}

struct Active {
    path: PathBuf,
    opened: Instant,
    len: u64,
}

pub(crate) struct ReplayBatch {
    pub records: Vec<UsageRecord>,
    path: PathBuf,
    end: u64,
    file_len: u64,
}

enum ReadOutcome {
    Batch(ReplayBatch),
    /// The record starting at this offset cannot be replayed.
    BadRecord {
        at: u64,
        reason: String,
    },
    /// The file cannot be positioned at all (unreadable checkpoint).
    Corrupt(String),
}

impl Wal {
    pub fn new(path: &str) -> Self {
        Self {
            legacy: path.into(),
            directory: format!("{path}.segments").into(),
            max_bytes: MAX_DISK_BYTES,
            active: Mutex::new(None),
        }
    }

    async fn files(&self) -> io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        match tokio::fs::metadata(&self.legacy).await {
            Ok(_) => files.push(self.legacy.clone()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut entries = match tokio::fs::read_dir(&self.directory).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(files),
            Err(e) => return Err(e),
        };
        let mut segments = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            if entry.path().extension().is_some_and(|e| e == "jsonl") {
                segments.push(entry.path());
            }
        }
        segments.sort();
        files.extend(segments);
        Ok(files)
    }

    fn take_active(&self) -> Option<Active> {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn set_active(&self, active: Option<Active>) {
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = active;
    }

    /// Refuse new spill at the cap; never evict older accounting silently.
    pub async fn append(&self, records: &[UsageRecord]) -> io::Result<()> {
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record)?;
            bytes.push(b'\n');
        }
        bytes.push(b'\n');
        if bytes.len() > REPLAY_BYTES {
            return Err(io::Error::other("telemetry spill batch exceeds 1 MiB"));
        }
        let files = self.files().await?;
        let mut total = bytes.len() as u64;
        for path in &files {
            total = total.saturating_add(tokio::fs::metadata(path).await?.len());
        }
        if total > self.max_bytes {
            return Err(io::Error::other("telemetry WAL disk/segment limit reached"));
        }

        // Taken out and only put back after a complete write, so a failed
        // write leaves its (possibly torn) segment closed to further appends.
        if let Some(mut active) = self.take_active() {
            let fits = active.len + bytes.len() as u64 <= SEGMENT_TARGET_BYTES
                && active.opened.elapsed() < SEGMENT_MAX_AGE;
            if fits {
                // Replay deletes a fully drained segment; then roll a new one.
                match tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(&active.path)
                    .await
                {
                    Ok(mut file) => {
                        file.write_all(&bytes).await?;
                        file.sync_data().await?;
                        active.len += bytes.len() as u64;
                        self.set_active(Some(active));
                        return Ok(());
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }

        let segments = files.iter().filter(|p| **p != self.legacy).count();
        if segments >= MAX_SEGMENTS {
            return Err(io::Error::other("telemetry WAL disk/segment limit reached"));
        }
        tokio::fs::create_dir_all(&self.directory).await?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = self
            .directory
            .join(format!("{stamp:020}-{}.jsonl", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_data().await?;
        self.set_active(Some(Active {
            path,
            opened: Instant::now(),
            len: bytes.len() as u64,
        }));
        Ok(())
    }

    /// Read one spilled insert batch (at most 1 MiB and 500 records, including
    /// when the legacy WAL is huge). A record that cannot be replayed is moved
    /// to `<name>.corrupt` and skipped, so replay never stalls on it and the
    /// valid records around it still replay.
    pub async fn next_batch(&self) -> io::Result<Option<ReplayBatch>> {
        for path in self.files().await? {
            loop {
                match read_batch(&path).await? {
                    ReadOutcome::Batch(batch) => return Ok(Some(batch)),
                    ReadOutcome::BadRecord { at, reason } => {
                        quarantine_record(&path, at, &reason).await?;
                    }
                    ReadOutcome::Corrupt(reason) => {
                        quarantine(&path, &reason).await?;
                        break;
                    }
                }
            }
        }
        Ok(None)
    }

    /// Persist progress only after the insert succeeds. A crash in between can
    /// replay that batch twice; failed checkpoint writes keep the data intact.
    pub async fn commit(&self, batch: &ReplayBatch) -> io::Result<()> {
        if batch.end == batch.file_len {
            tokio::fs::remove_file(&batch.path).await?;
            match tokio::fs::remove_file(checkpoint(&batch.path)).await {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        } else {
            write_checkpoint(&batch.path, batch.end).await?;
        }
        Ok(())
    }
}

async fn write_checkpoint(path: &Path, offset: u64) -> io::Result<()> {
    let target = checkpoint(path);
    let temporary = target.with_extension("offset.tmp");
    let mut file = tokio::fs::File::create(&temporary).await?;
    file.write_all(offset.to_string().as_bytes()).await?;
    file.sync_data().await?;
    drop(file);
    tokio::fs::rename(temporary, target).await
}

async fn read_batch(path: &Path) -> io::Result<ReadOutcome> {
    let mut file = tokio::fs::File::open(path).await?;
    let file_len = file.metadata().await?.len();
    let offset = match tokio::fs::read_to_string(checkpoint(path)).await {
        Ok(text) => match text.trim().parse::<u64>() {
            Ok(offset) if offset <= file_len => offset,
            _ => return Ok(ReadOutcome::Corrupt(format!("invalid checkpoint {text:?}"))),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    file.seek(io::SeekFrom::Start(offset)).await?;
    let mut reader = tokio::io::BufReader::new(file.take(REPLAY_BYTES as u64));
    let mut records = Vec::new();
    let mut consumed = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await? as u64;
        if n == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            if offset + consumed + n < file_len {
                // The byte window ended mid-record.
                if records.is_empty() {
                    return Ok(ReadOutcome::BadRecord {
                        at: offset + consumed,
                        reason: "record exceeds replay byte limit".into(),
                    });
                }
                break;
            }
            // Unterminated at EOF: a crash mid-append. Keep it only if it is
            // nonetheless a complete record.
            match parse_record(&line) {
                Ok(record) => records.push(record),
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    bytes = n,
                    error = %e,
                    "discarding torn record at end of telemetry WAL"
                ),
            }
            consumed += n;
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            consumed += n;
            if records.is_empty() {
                continue;
            }
            break;
        }
        match parse_record(&line) {
            Ok(record) => records.push(record),
            // Replay what precedes the bad line; the next read quarantines.
            Err(_) if !records.is_empty() => break,
            Err(e) => {
                return Ok(ReadOutcome::BadRecord {
                    at: offset + consumed,
                    reason: format!("unparseable record: {e}"),
                })
            }
        }
        consumed += n;
        if records.len() == super::BATCH_MAX {
            break;
        }
    }
    Ok(ReadOutcome::Batch(ReplayBatch {
        records,
        path: path.to_path_buf(),
        end: offset + consumed,
        file_len,
    }))
}

/// Parse one WAL line. Non-finite `f64`s were written as `null`; they read back
/// as `0.0` so an already-poisoned WAL still replays.
fn parse_record(line: &[u8]) -> serde_json::Result<UsageRecord> {
    let mut value: serde_json::Value = serde_json::from_slice(line)?;
    if let Some(object) = value.as_object_mut() {
        for field in FLOAT_FIELDS {
            if object.get(field).is_some_and(serde_json::Value::is_null) {
                object.insert(field.into(), 0.0.into());
            }
        }
    }
    serde_json::from_value(value)
}

/// Append the line starting at `at` (through its newline, or EOF) to
/// `<name>.corrupt`, then checkpoint past it. Copied in chunks because an
/// oversized line is exactly the case that does not fit the replay window.
/// A crash between the copy and the checkpoint only duplicates the line in
/// the `.corrupt` file.
async fn quarantine_record(path: &Path, at: u64, reason: &str) -> io::Result<()> {
    let target = with_suffix(path, ".corrupt");
    let mut source = tokio::fs::File::open(path).await?;
    source.seek(io::SeekFrom::Start(at)).await?;
    let mut reader = tokio::io::BufReader::new(source);
    let mut sink = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target)
        .await?;
    let mut end = at;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            break;
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        sink.write_all(&chunk[..take]).await?;
        reader.consume(take);
        end += take as u64;
        if done {
            break;
        }
    }
    sink.sync_data().await?;
    tracing::error!(
        path = %path.display(),
        moved_to = %target.display(),
        offset = at,
        bytes = end - at,
        reason,
        "telemetry WAL record unreadable; moved aside, replay continues"
    );
    write_checkpoint(path, end).await
}

async fn quarantine(path: &Path, reason: &str) -> io::Result<()> {
    let target = with_suffix(path, ".corrupt");
    tracing::error!(
        path = %path.display(),
        moved_to = %target.display(),
        reason,
        "telemetry WAL file unreadable; moved aside, replay continues"
    );
    tokio::fs::rename(path, &target).await?;
    // Keep the checkpoint beside it: it marks which rows were already inserted.
    match tokio::fs::rename(checkpoint(path), checkpoint(&target)).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn checkpoint(path: &Path) -> PathBuf {
    with_suffix(path, ".offset")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> UsageRecord {
        serde_json::from_value(serde_json::json!({
            "request_id":uuid::Uuid::new_v4(), "tenant_id":uuid::Uuid::nil(), "key_id":uuid::Uuid::nil(),
            "model":"test", "admission":"ok", "weight":1, "input_tokens":1, "output_tokens":1,
            "estimated_tokens":2, "queue_wait_ms":0, "ttft_ms":0, "total_ms":1,
            "status_code":200, "cache_status":"off", "cost_usd":0.0, "ts_ms":0
        })).unwrap()
    }

    async fn fixture() -> (PathBuf, Wal) {
        let directory =
            std::env::temp_dir().join(format!("obleth-wal-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal = Wal::new(directory.join("usage.jsonl").to_str().unwrap());
        (directory, wal)
    }

    async fn segment_count(wal: &Wal) -> usize {
        let mut entries = tokio::fs::read_dir(&wal.directory).await.unwrap();
        let mut n = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            if e.path().extension().is_some_and(|x| x == "jsonl") {
                n += 1;
            }
        }
        n
    }

    async fn drain(wal: &Wal) -> Vec<UsageRecord> {
        let mut out = Vec::new();
        while let Some(batch) = wal.next_batch().await.unwrap() {
            out.extend(batch.records.iter().cloned());
            wal.commit(&batch).await.unwrap();
        }
        out
    }

    #[tokio::test]
    async fn small_appends_share_segments_and_replay_in_order() {
        let (directory, wal) = fixture().await;
        let mut written = Vec::new();
        for _ in 0..3000 {
            let rec = record();
            wal.append(std::slice::from_ref(&rec)).await.unwrap();
            written.push(rec.request_id);
        }
        assert!(segment_count(&wal).await <= 4);
        let replayed: Vec<_> = drain(&wal).await.iter().map(|r| r.request_id).collect();
        assert_eq!(replayed, written);
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn torn_final_line_is_discarded() {
        let (directory, wal) = fixture().await;
        let good = record();
        let mut bytes = format!("{}\n", serde_json::to_string(&good).unwrap()).into_bytes();
        bytes.extend_from_slice(br#"{"request_id":"#);
        tokio::fs::write(&wal.legacy, &bytes).await.unwrap();
        let replayed = drain(&wal).await;
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].request_id, good.request_id);
        assert!(tokio::fs::metadata(&wal.legacy).await.is_err());
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_middle_line_is_moved_aside_and_rest_of_segment_replays() {
        let (directory, wal) = fixture().await;
        let before = record();
        let after = record();
        let mut bytes = format!("{}\n", serde_json::to_string(&before).unwrap()).into_bytes();
        bytes.extend_from_slice(b"not json\n");
        bytes.extend_from_slice(format!("{}\n", serde_json::to_string(&after).unwrap()).as_bytes());
        tokio::fs::write(&wal.legacy, &bytes).await.unwrap();
        let later = record();
        wal.append(std::slice::from_ref(&later)).await.unwrap();
        let replayed: Vec<_> = drain(&wal).await.iter().map(|r| r.request_id).collect();
        assert_eq!(
            replayed,
            vec![before.request_id, after.request_id, later.request_id]
        );
        let quarantined = with_suffix(&wal.legacy, ".corrupt");
        assert_eq!(tokio::fs::read(&quarantined).await.unwrap(), b"not json\n");
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn null_float_fields_from_old_wal_replay_as_zero() {
        let (directory, wal) = fixture().await;
        let mut value = serde_json::to_value(record()).unwrap();
        value["energy_wh"] = serde_json::Value::Null;
        value["co2_g"] = serde_json::Value::Null;
        tokio::fs::write(&wal.legacy, format!("{value}\n"))
            .await
            .unwrap();
        let replayed = drain(&wal).await;
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].energy_wh, 0.0);
        assert_eq!(replayed[0].co2_g, 0.0);
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn non_finite_figures_round_trip_through_the_wal() {
        let (directory, wal) = fixture().await;
        let mut rec = record();
        rec.energy_wh = f64::NAN;
        rec.cost_usd = f64::INFINITY;
        wal.append(std::slice::from_ref(&rec)).await.unwrap();
        let replayed = drain(&wal).await;
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].energy_wh, 0.0);
        assert_eq!(replayed[0].cost_usd, 0.0);
        tokio::fs::remove_dir_all(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn legacy_replay_is_bounded_and_checkpoint_survives_restart() {
        let (directory, wal) = fixture().await;
        let line = format!("{}\n", serde_json::to_string(&record()).unwrap());
        tokio::fs::write(&wal.legacy, line.repeat(1201))
            .await
            .unwrap();
        let first = wal.next_batch().await.unwrap().unwrap();
        assert_eq!(first.records.len(), 500);
        // An unacknowledged insert is replayed rather than lost.
        assert_eq!(wal.next_batch().await.unwrap().unwrap().end, first.end);
        wal.commit(&first).await.unwrap();
        let restarted = Wal::new(wal.legacy.to_str().unwrap());
        let second = restarted.next_batch().await.unwrap().unwrap();
        assert_eq!(second.records.len(), 500);
        assert!(second.end > first.end);
        restarted.commit(&second).await.unwrap();
        let last = restarted.next_batch().await.unwrap().unwrap();
        assert_eq!(last.records.len(), 201);
        restarted.commit(&last).await.unwrap();
        assert!(restarted.next_batch().await.unwrap().is_none());
        tokio::fs::remove_dir(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn cap_preserves_existing_spill_and_segments_replay() {
        let (directory, mut wal) = fixture().await;
        let rec = record();
        wal.append(std::slice::from_ref(&rec)).await.unwrap();
        wal.max_bytes = 1;
        assert!(wal.append(&[record()]).await.is_err());
        let batch = wal.next_batch().await.unwrap().unwrap();
        assert_eq!(batch.records[0].request_id, rec.request_id);
        wal.commit(&batch).await.unwrap();
        assert!(wal.next_batch().await.unwrap().is_none());
        tokio::fs::remove_dir(&wal.directory).await.unwrap();
        tokio::fs::remove_dir(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_or_oversized_legacy_record_is_retained_aside() {
        let (directory, wal) = fixture().await;
        let quarantined = with_suffix(&wal.legacy, ".corrupt");
        let good = record();
        let good_line = format!("{}\n", serde_json::to_string(&good).unwrap());
        for bad in [
            b"invalid\n".to_vec(),
            [vec![b'x'; REPLAY_BYTES + 1], vec![b'\n']].concat(),
        ] {
            let mut bytes = bad.clone();
            bytes.extend_from_slice(good_line.as_bytes());
            tokio::fs::write(&wal.legacy, &bytes).await.unwrap();
            let replayed = drain(&wal).await;
            assert_eq!(replayed.len(), 1);
            assert_eq!(replayed[0].request_id, good.request_id);
            assert_eq!(tokio::fs::read(&quarantined).await.unwrap(), bad);
            assert!(tokio::fs::metadata(&wal.legacy).await.is_err());
            tokio::fs::remove_file(&quarantined).await.unwrap();
        }
        tokio::fs::remove_dir(&directory).await.unwrap();
    }
}
