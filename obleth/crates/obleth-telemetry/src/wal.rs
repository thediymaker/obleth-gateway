//! Bounded spill segments and checkpointed, at-least-once replay. The original
//! single WAL is read first for upgrade compatibility. One flusher owns this WAL.

use obleth_config::UsageRecord;
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const MAX_DISK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1024;
const REPLAY_BYTES: usize = 1024 * 1024;

pub(crate) struct Wal {
    legacy: PathBuf,
    directory: PathBuf,
    max_bytes: u64,
}

pub(crate) struct ReplayBatch {
    pub records: Vec<UsageRecord>,
    path: PathBuf,
    end: u64,
    file_len: u64,
}

impl Wal {
    pub fn new(path: &str) -> Self {
        Self {
            legacy: path.into(),
            directory: format!("{path}.segments").into(),
            max_bytes: MAX_DISK_BYTES,
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

    /// Refuse new spill at the cap; never evict older accounting silently.
    pub async fn append(&self, records: &[UsageRecord]) -> io::Result<()> {
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record)?;
            bytes.push(b'\n');
            if bytes.len() > REPLAY_BYTES {
                return Err(io::Error::other("telemetry spill batch exceeds 1 MiB"));
            }
        }
        let files = self.files().await?;
        let mut total = bytes.len() as u64;
        for path in &files {
            total = total.saturating_add(tokio::fs::metadata(path).await?.len());
        }
        if total > self.max_bytes || files.len() >= MAX_SEGMENTS {
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
            .open(path)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_data().await
    }

    /// Read at most 1 MiB and 500 records, including when the legacy WAL is huge.
    pub async fn next_batch(&self) -> io::Result<Option<ReplayBatch>> {
        let Some(path) = self.files().await?.into_iter().next() else {
            return Ok(None);
        };
        let mut file = tokio::fs::File::open(&path).await?;
        let file_len = file.metadata().await?.len();
        let offset = match tokio::fs::read_to_string(checkpoint(&path)).await {
            Ok(text) => text.parse::<u64>().map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        if offset > file_len {
            return Err(io::Error::other("WAL checkpoint exceeds file length"));
        }
        file.seek(io::SeekFrom::Start(offset)).await?;
        let mut bytes = Vec::new();
        file.take(REPLAY_BYTES as u64)
            .read_to_end(&mut bytes)
            .await?;
        let at_eof = offset + bytes.len() as u64 == file_len;
        let mut records = Vec::new();
        let mut consumed = 0;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if !line.ends_with(b"\n") && !at_eof {
                break;
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                records.push(serde_json::from_slice(line)?);
            }
            consumed += line.len();
            if records.len() == super::BATCH_MAX {
                break;
            }
        }
        if consumed == 0 && !bytes.is_empty() {
            return Err(io::Error::other("WAL record exceeds replay byte limit"));
        }
        Ok(Some(ReplayBatch {
            records,
            path,
            end: offset + consumed as u64,
            file_len,
        }))
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
            let target = checkpoint(&batch.path);
            let temporary = target.with_extension("offset.tmp");
            let mut file = tokio::fs::File::create(&temporary).await?;
            file.write_all(batch.end.to_string().as_bytes()).await?;
            file.sync_data().await?;
            drop(file);
            tokio::fs::rename(temporary, target).await?;
        }
        Ok(())
    }
}

fn checkpoint(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".offset");
    name.into()
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
    async fn corrupt_or_oversized_legacy_record_is_retained() {
        let (directory, wal) = fixture().await;
        for bytes in [b"invalid\n".to_vec(), vec![b'x'; REPLAY_BYTES + 1]] {
            tokio::fs::write(&wal.legacy, &bytes).await.unwrap();
            assert!(wal.next_batch().await.is_err());
            assert_eq!(
                tokio::fs::metadata(&wal.legacy).await.unwrap().len(),
                bytes.len() as u64
            );
        }
        tokio::fs::remove_file(&wal.legacy).await.unwrap();
        tokio::fs::remove_dir(&directory).await.unwrap();
    }
}
