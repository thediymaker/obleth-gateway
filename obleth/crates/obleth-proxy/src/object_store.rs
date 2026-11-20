//! The gateway's object store: one S3 or S3-compatible bucket for bytes that
//! do not belong in Postgres. Each feature keeps its objects under its own
//! key prefix (batches use `batches/`).
//!
//! A small client (put, get, ranged get, delete) signed with AWS Signature
//! Version 4, using path-style addresses (`<endpoint>/<bucket>/<key>`).
//! Configured with `OBLETH_S3_BUCKET`, `OBLETH_S3_ENDPOINT`,
//! `OBLETH_S3_REGION`, `OBLETH_S3_ACCESS_KEY_ID` and
//! `OBLETH_S3_SECRET_ACCESS_KEY`; setting the bucket turns it on.

use std::fmt;
use std::time::Duration;

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Attempts per request: a store that blips (a 5xx, a dropped connection)
/// gets two more tries before the caller sees the error.
const ATTEMPTS: u32 = 3;

/// Every request but large transfers; see [`transfer_time`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long moving `bytes` may take: a minute, and a second for every MiB more.
fn transfer_time(bytes: usize) -> Duration {
    REQUEST_TIMEOUT + Duration::from_secs((bytes / (1024 * 1024)) as u64)
}

#[derive(Debug)]
pub(crate) enum ObjectError {
    /// The object does not exist.
    NotFound,
    /// The store answered with an error status.
    Status(u16, String),
    /// The store could not be reached, or the answer could not be read.
    Transport(String),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectError::NotFound => write!(f, "object not found"),
            ObjectError::Status(code, body) => write!(f, "object store answered {code}: {body}"),
            ObjectError::Transport(e) => write!(f, "object store unreachable: {e}"),
        }
    }
}

impl ObjectError {
    fn retryable(&self) -> bool {
        match self {
            ObjectError::NotFound => false,
            ObjectError::Status(code, _) => *code == 429 || *code >= 500,
            ObjectError::Transport(_) => true,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ObjectStore {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    /// `host[:port]` as the request sends it, for signing.
    host: String,
    bucket: String,
    /// Prepended to every key, without slashes at either end. Empty but in
    /// tests, which each work under a prefix of their own.
    prefix: String,
    region: String,
    access_key: String,
    secret_key: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// S3's rule for bucket names, short of the corner cases: 3-63 lower-case
/// letters, digits, '-' and '.'.
fn check_bucket_name(bucket: &str) -> Result<(), String> {
    let ok = (3..=63).contains(&bucket.len())
        && bucket
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "OBLETH_S3_BUCKET: {bucket:?} is not a bucket name (3-63 lower-case letters, digits, '-' or '.')"
        ))
    }
}

impl ObjectStore {
    /// The store from the environment: `None` when `OBLETH_S3_BUCKET` is
    /// unset, an error when it is set but the rest of the settings are not
    /// usable. Without `OBLETH_S3_ENDPOINT` the bucket is on AWS.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Some(bucket) = env("OBLETH_S3_BUCKET") else {
            return Ok(None);
        };
        let region = env("OBLETH_S3_REGION").unwrap_or_else(|| "us-east-1".into());
        let endpoint = env("OBLETH_S3_ENDPOINT")
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let access_key = env("OBLETH_S3_ACCESS_KEY_ID")
            .ok_or("OBLETH_S3_BUCKET is set but OBLETH_S3_ACCESS_KEY_ID is not")?;
        let secret_key = env("OBLETH_S3_SECRET_ACCESS_KEY")
            .ok_or("OBLETH_S3_BUCKET is set but OBLETH_S3_SECRET_ACCESS_KEY is not")?;
        Self::new(&endpoint, bucket, region, access_key, secret_key).map(Some)
    }

    pub(crate) fn new(
        endpoint: &str,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
    ) -> Result<Self, String> {
        check_bucket_name(&bucket)?;
        let endpoint = reqwest::Url::parse(endpoint)
            .map_err(|e| format!("OBLETH_S3_ENDPOINT {endpoint:?} is not a URL: {e}"))?;
        let host = match (endpoint.host_str(), endpoint.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            (None, _) => return Err(format!("OBLETH_S3_ENDPOINT {endpoint} has no host")),
        };
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(format!(
                "OBLETH_S3_ENDPOINT {endpoint} must be http or https"
            ));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("object store client: {e}"))?;
        Ok(ObjectStore {
            http,
            endpoint,
            host,
            bucket,
            prefix: String::new(),
            region,
            access_key,
            secret_key,
        })
    }

    /// Check the store in the background: a write, a read and a delete. The
    /// outcome is logged; nothing waits for it.
    pub(crate) fn spawn_check(&self) {
        let store = self.clone();
        tokio::spawn(async move {
            match store.check().await {
                Ok(()) => tracing::info!(bucket = %store.describe(), "object store reachable"),
                Err(e) => tracing::error!(
                    bucket = %store.describe(), error = %e,
                    "object store check failed: features that keep bytes there will fail until it works"
                ),
            }
        });
    }

    /// Where objects go, for logs.
    pub(crate) fn describe(&self) -> String {
        let base = self.endpoint.as_str().trim_end_matches('/');
        if self.prefix.is_empty() {
            format!("{base}/{}", self.bucket)
        } else {
            format!("{base}/{}/{}", self.bucket, self.prefix)
        }
    }

    /// The URL path of `key`: the endpoint's own path, the bucket, the
    /// prefix and the key, each segment percent-encoded as SigV4 requires.
    fn path(&self, key: &str) -> String {
        let mut path = self.endpoint.path().trim_end_matches('/').to_string();
        let full = if self.prefix.is_empty() {
            format!("{}/{key}", self.bucket)
        } else {
            format!("{}/{}/{key}", self.bucket, self.prefix)
        };
        for segment in full.split('/') {
            path.push('/');
            path.push_str(&uri_encode(segment));
        }
        path
    }

    pub(crate) async fn put(&self, key: &str, body: Bytes) -> Result<(), ObjectError> {
        let timeout = transfer_time(body.len());
        self.send(reqwest::Method::PUT, key, None, body, timeout)
            .await
            .map(drop)
    }

    pub(crate) async fn get(&self, key: &str) -> Result<Bytes, ObjectError> {
        self.send(
            reqwest::Method::GET,
            key,
            None,
            Bytes::new(),
            REQUEST_TIMEOUT,
        )
        .await
    }

    /// `len` bytes of `key` from `start`.
    pub(crate) async fn get_range(
        &self,
        key: &str,
        start: u64,
        len: u64,
    ) -> Result<Bytes, ObjectError> {
        if len == 0 {
            return Ok(Bytes::new());
        }
        let range = format!("bytes={start}-{}", start + len - 1);
        let bytes = self
            .send(
                reqwest::Method::GET,
                key,
                Some(&range),
                Bytes::new(),
                transfer_time(len as usize),
            )
            .await?;
        if bytes.len() as u64 != len {
            return Err(ObjectError::Transport(format!(
                "asked for {len} bytes of {key}, got {}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Delete `key`; a missing object counts as deleted.
    pub(crate) async fn delete(&self, key: &str) -> Result<(), ObjectError> {
        match self
            .send(
                reqwest::Method::DELETE,
                key,
                None,
                Bytes::new(),
                REQUEST_TIMEOUT,
            )
            .await
        {
            Ok(_) | Err(ObjectError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Write, read back and delete a small object: proves the endpoint, the
    /// credentials and the bucket at startup.
    pub(crate) async fn check(&self) -> Result<(), ObjectError> {
        let key = format!("check/{}", crate::replicas::instance_id());
        let probe = Bytes::from_static(b"obleth object store check");
        self.put(&key, probe.clone()).await?;
        let back = self.get(&key).await?;
        self.delete(&key).await?;
        if back != probe {
            return Err(ObjectError::Transport(
                "the object read back differs from the one written".into(),
            ));
        }
        Ok(())
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        range: Option<&str>,
        body: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ObjectError> {
        let mut attempt = 1;
        loop {
            match self
                .send_once(method.clone(), key, range, body.clone(), timeout)
                .await
            {
                Err(e) if e.retryable() && attempt < ATTEMPTS => {
                    tracing::debug!(error = %e, key, attempt, "object store request failed; trying again");
                    tokio::time::sleep(Duration::from_millis(250 * 4u64.pow(attempt - 1))).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn send_once(
        &self,
        method: reqwest::Method,
        key: &str,
        range: Option<&str>,
        body: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ObjectError> {
        let path = self.path(key);
        let payload_hash = hex::encode(Sha256::digest(&body));
        let mut extra = Vec::new();
        if let Some(r) = range {
            extra.push(("range", r));
        }
        let signed = sign(
            &SigningInput {
                method: method.as_str(),
                host: &self.host,
                path: &path,
                headers: &extra,
                payload_hash: &payload_hash,
                region: &self.region,
                access_key: &self.access_key,
                secret_key: &self.secret_key,
            },
            Utc::now(),
        );
        let mut url = self.endpoint.clone();
        url.set_path(&path);
        let mut req = self.http.request(method, url).timeout(timeout);
        for (name, value) in &extra {
            req = req.header(*name, *value);
        }
        for (name, value) in &signed {
            req = req.header(*name, value);
        }
        if !body.is_empty() {
            req = req.body(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ObjectError::Transport(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ObjectError::NotFound);
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ObjectError::Transport(e.to_string()))?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
            return Err(ObjectError::Status(status.as_u16(), text));
        }
        Ok(bytes)
    }
}

/// Percent-encode everything but RFC 3986 unreserved characters.
fn uri_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct SigningInput<'a> {
    method: &'a str,
    host: &'a str,
    /// Already encoded.
    path: &'a str,
    /// Lower-case names; signed along with host and the x-amz headers.
    headers: &'a [(&'a str, &'a str)],
    payload_hash: &'a str,
    region: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// The headers that sign a request without a query string:
/// `x-amz-content-sha256`, `x-amz-date` and `authorization`.
fn sign(input: &SigningInput<'_>, now: DateTime<Utc>) -> Vec<(&'static str, String)> {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = &amz_date[..8];
    let mut headers: Vec<(&str, &str)> = vec![
        ("host", input.host),
        ("x-amz-content-sha256", input.payload_hash),
        ("x-amz-date", &amz_date),
    ];
    headers.extend_from_slice(input.headers);
    headers.sort_by(|a, b| a.0.cmp(b.0));
    let canonical_headers: String = headers
        .iter()
        .map(|(n, v)| format!("{n}:{}\n", v.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n\n{canonical_headers}\n{signed_headers}\n{}",
        input.method, input.path, input.payload_hash
    );
    let scope = format!("{date}/{}/s3/aws4_request", input.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let mut key = hmac(format!("AWS4{}", input.secret_key).as_bytes(), date);
    for part in [input.region, "s3", "aws4_request"] {
        key = hmac(&key, part);
    }
    let signature = hex::encode(hmac(&key, &string_to_sign));
    vec![
        ("x-amz-content-sha256", input.payload_hash.to_string()),
        ("x-amz-date", amz_date.clone()),
        (
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                input.access_key
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The "GET Object" example of AWS's Signature Version 4 documentation
    /// for S3 (header-based auth), signature included.
    #[test]
    fn signing_matches_the_aws_example() {
        let empty = hex::encode(Sha256::digest(b""));
        let headers = sign(
            &SigningInput {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                headers: &[("range", "bytes=0-9")],
                payload_hash: &empty,
                region: "us-east-1",
                access_key: "AKIAIOSFODNN7EXAMPLE",
                secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            },
            Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
        );
        let auth = &headers
            .iter()
            .find(|(n, _)| *n == "authorization")
            .unwrap()
            .1;
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn bucket_names_follow_the_s3_rule() {
        assert!(check_bucket_name("obleth-dev").is_ok());
        assert!(check_bucket_name("Upper").is_err());
        assert!(check_bucket_name("s3://obleth").is_err());
        assert!(check_bucket_name("ab").is_err());
    }

    #[test]
    fn paths_are_path_style() {
        let s = ObjectStore::new(
            "https://store.example:9021/base/",
            "bkt".into(),
            "us-east-1".into(),
            "a".into(),
            "s".into(),
        )
        .unwrap();
        assert_eq!(s.host, "store.example:9021");
        assert_eq!(
            s.path("batches/files/f1/00000"),
            "/base/bkt/batches/files/f1/00000"
        );
        assert_eq!(s.describe(), "https://store.example:9021/base/bkt");
        let t = ObjectStore {
            prefix: "test-1".into(),
            ..s
        };
        assert_eq!(t.path("k"), "/base/bkt/test-1/k");
    }

    /// A store for integration tests, from `OBLETH_TEST_S3_BUCKET`,
    /// `OBLETH_TEST_S3_ENDPOINT`, `OBLETH_TEST_S3_ACCESS_KEY_ID` and
    /// `OBLETH_TEST_S3_SECRET_ACCESS_KEY`, under a fresh prefix so runs do
    /// not see each other's objects. None when the bucket is unset.
    pub(crate) fn test_store() -> Option<ObjectStore> {
        let bucket = env("OBLETH_TEST_S3_BUCKET")?;
        let store = ObjectStore::new(
            &env("OBLETH_TEST_S3_ENDPOINT").expect("OBLETH_TEST_S3_ENDPOINT"),
            bucket,
            env("OBLETH_TEST_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            env("OBLETH_TEST_S3_ACCESS_KEY_ID").expect("OBLETH_TEST_S3_ACCESS_KEY_ID"),
            env("OBLETH_TEST_S3_SECRET_ACCESS_KEY").expect("OBLETH_TEST_S3_SECRET_ACCESS_KEY"),
        )
        .unwrap();
        Some(ObjectStore {
            prefix: format!("test-{}", uuid::Uuid::new_v4().simple()),
            ..store
        })
    }

    /// Integration test; runs only when `OBLETH_TEST_S3_BUCKET` is set.
    #[tokio::test]
    async fn objects_are_written_read_in_ranges_and_deleted() {
        let Some(b) = test_store() else {
            eprintln!("skipping: set OBLETH_TEST_S3_BUCKET to run");
            return;
        };
        b.check().await.expect("check");
        let body = Bytes::from_static(b"0123456789abcdef");
        b.put("x/y", body.clone()).await.unwrap();
        assert_eq!(b.get("x/y").await.unwrap(), body);
        assert_eq!(&b.get_range("x/y", 3, 4).await.unwrap()[..], b"3456");
        b.delete("x/y").await.unwrap();
        assert!(matches!(b.get("x/y").await, Err(ObjectError::NotFound)));
        // Deleting what is not there is not an error.
        b.delete("x/y").await.unwrap();
    }
}
