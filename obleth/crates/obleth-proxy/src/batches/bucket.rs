//! The bucket batches keep their files and answers in: a small S3 client
//! (put, get, ranged get, delete) signed with AWS Signature Version 4, for
//! S3 and S3-compatible stores, using path-style addresses
//! (`<endpoint>/<bucket>/<key>`).
//!
//! Configured with `OBLETH_BATCH_BUCKET=s3://<bucket>[/<prefix>]` and the
//! standard `AWS_ENDPOINT_URL`, `AWS_REGION`, `AWS_ACCESS_KEY_ID` and
//! `AWS_SECRET_ACCESS_KEY`.

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
pub(crate) enum BucketError {
    /// The object does not exist.
    NotFound,
    /// The store answered with an error status.
    Status(u16, String),
    /// The store could not be reached, or the answer could not be read.
    Transport(String),
}

impl fmt::Display for BucketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BucketError::NotFound => write!(f, "object not found"),
            BucketError::Status(code, body) => write!(f, "object store answered {code}: {body}"),
            BucketError::Transport(e) => write!(f, "object store unreachable: {e}"),
        }
    }
}

impl BucketError {
    fn retryable(&self) -> bool {
        match self {
            BucketError::NotFound => false,
            BucketError::Status(code, _) => *code == 429 || *code >= 500,
            BucketError::Transport(_) => true,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Bucket {
    http: reqwest::Client,
    endpoint: reqwest::Url,
    /// `host[:port]` as the request sends it, for signing.
    host: String,
    bucket: String,
    /// Prepended to every key, without slashes at either end; may be empty.
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

/// `s3://<bucket>[/<prefix>]` as (bucket, prefix).
fn parse_location(raw: &str) -> Result<(String, String), String> {
    let rest = raw
        .trim()
        .strip_prefix("s3://")
        .ok_or("OBLETH_BATCH_BUCKET must look like s3://<bucket>[/<prefix>]")?;
    let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    let bucket_ok = (3..=63).contains(&bucket.len())
        && bucket
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    if !bucket_ok {
        return Err(format!(
            "OBLETH_BATCH_BUCKET: {bucket:?} is not a bucket name (3-63 lower-case letters, digits, '-' or '.')"
        ));
    }
    let prefix = prefix.trim_matches('/');
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(format!(
            "OBLETH_BATCH_BUCKET: the prefix {prefix:?} may hold only letters, digits, '-', '_', '.' and '/'"
        ));
    }
    Ok((bucket.to_string(), prefix.to_string()))
}

impl Bucket {
    /// The bucket from the environment: `None` when `OBLETH_BATCH_BUCKET` is
    /// unset, an error when it is set but the rest of the settings are not
    /// usable.
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let Some(location) = env("OBLETH_BATCH_BUCKET") else {
            return Ok(None);
        };
        let (bucket, prefix) = parse_location(&location)?;
        let region = env("AWS_REGION")
            .or_else(|| env("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| "us-east-1".into());
        let endpoint = env("AWS_ENDPOINT_URL_S3")
            .or_else(|| env("AWS_ENDPOINT_URL"))
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let access_key = env("AWS_ACCESS_KEY_ID")
            .ok_or("OBLETH_BATCH_BUCKET is set but AWS_ACCESS_KEY_ID is not")?;
        let secret_key = env("AWS_SECRET_ACCESS_KEY")
            .ok_or("OBLETH_BATCH_BUCKET is set but AWS_SECRET_ACCESS_KEY is not")?;
        Self::new(&endpoint, bucket, prefix, region, access_key, secret_key).map(Some)
    }

    pub(crate) fn new(
        endpoint: &str,
        bucket: String,
        prefix: String,
        region: String,
        access_key: String,
        secret_key: String,
    ) -> Result<Self, String> {
        let endpoint = reqwest::Url::parse(endpoint)
            .map_err(|e| format!("AWS_ENDPOINT_URL {endpoint:?} is not a URL: {e}"))?;
        let host = match (endpoint.host_str(), endpoint.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            (None, _) => return Err(format!("AWS_ENDPOINT_URL {endpoint} has no host")),
        };
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(format!("AWS_ENDPOINT_URL {endpoint} must be http or https"));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("object store client: {e}"))?;
        Ok(Bucket {
            http,
            endpoint,
            host,
            bucket,
            prefix,
            region,
            access_key,
            secret_key,
        })
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

    pub(crate) async fn put(&self, key: &str, body: Bytes) -> Result<(), BucketError> {
        let timeout = transfer_time(body.len());
        self.send(reqwest::Method::PUT, key, None, body, timeout)
            .await
            .map(drop)
    }

    pub(crate) async fn get(&self, key: &str) -> Result<Bytes, BucketError> {
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
    ) -> Result<Bytes, BucketError> {
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
            return Err(BucketError::Transport(format!(
                "asked for {len} bytes of {key}, got {}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    /// Delete `key`; a missing object counts as deleted.
    pub(crate) async fn delete(&self, key: &str) -> Result<(), BucketError> {
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
            Ok(_) | Err(BucketError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Write, read back and delete a small object: proves the endpoint, the
    /// credentials and the bucket at startup.
    pub(crate) async fn check(&self) -> Result<(), BucketError> {
        let key = format!("check/{}", crate::replicas::instance_id());
        let probe = Bytes::from_static(b"obleth batch storage check");
        self.put(&key, probe.clone()).await?;
        let back = self.get(&key).await?;
        self.delete(&key).await?;
        if back != probe {
            return Err(BucketError::Transport(
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
    ) -> Result<Bytes, BucketError> {
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
    ) -> Result<Bytes, BucketError> {
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
            .map_err(|e| BucketError::Transport(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(BucketError::NotFound);
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| BucketError::Transport(e.to_string()))?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
            return Err(BucketError::Status(status.as_u16(), text));
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
    fn locations_name_a_bucket_and_an_optional_prefix() {
        assert_eq!(
            parse_location("s3://obleth-batches").unwrap(),
            ("obleth-batches".into(), "".into())
        );
        assert_eq!(
            parse_location("s3://obleth-batches/prod/").unwrap(),
            ("obleth-batches".into(), "prod".into())
        );
        assert!(parse_location("obleth-batches").is_err());
        assert!(parse_location("s3://Upper").is_err());
        assert!(parse_location("s3://ok-bucket/sp ace").is_err());
    }

    #[test]
    fn paths_are_path_style_under_the_prefix() {
        let b = Bucket::new(
            "https://store.example:9021/base/",
            "bkt".into(),
            "p/q".into(),
            "us-east-1".into(),
            "a".into(),
            "s".into(),
        )
        .unwrap();
        assert_eq!(b.host, "store.example:9021");
        assert_eq!(b.path("files/f1/00000"), "/base/bkt/p/q/files/f1/00000");
        assert_eq!(b.describe(), "https://store.example:9021/base/bkt/p/q");
    }

    /// A bucket for integration tests: `OBLETH_TEST_BUCKET` (s3://...) with
    /// the usual AWS_* settings, under a fresh prefix. None when unset.
    pub(crate) fn test_bucket() -> Option<Bucket> {
        let location = std::env::var("OBLETH_TEST_BUCKET").ok()?;
        let (bucket, prefix) = parse_location(&location).expect("OBLETH_TEST_BUCKET");
        let prefix = format!(
            "{}test-{}",
            if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
            uuid::Uuid::new_v4().simple()
        );
        Some(
            Bucket::new(
                &env("AWS_ENDPOINT_URL").expect("AWS_ENDPOINT_URL"),
                bucket,
                prefix,
                env("AWS_REGION").unwrap_or_else(|| "us-east-1".into()),
                env("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID"),
                env("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY"),
            )
            .unwrap(),
        )
    }

    /// Integration test; runs only when `OBLETH_TEST_BUCKET` is set.
    #[tokio::test]
    async fn objects_are_written_read_in_ranges_and_deleted() {
        let Some(b) = test_bucket() else {
            eprintln!("skipping: set OBLETH_TEST_BUCKET to run");
            return;
        };
        b.check().await.expect("check");
        let body = Bytes::from_static(b"0123456789abcdef");
        b.put("x/y", body.clone()).await.unwrap();
        assert_eq!(b.get("x/y").await.unwrap(), body);
        assert_eq!(&b.get_range("x/y", 3, 4).await.unwrap()[..], b"3456");
        b.delete("x/y").await.unwrap();
        assert!(matches!(b.get("x/y").await, Err(BucketError::NotFound)));
        // Deleting what is not there is not an error.
        b.delete("x/y").await.unwrap();
    }
}
