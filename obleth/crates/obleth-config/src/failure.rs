//! Why a request failed: whose side it was on, and the reason given.
//!
//! A status code alone says only that a request failed. A day with thousands
//! of 400s from one client that keeps sending a malformed request reads the
//! same as a day of crashed model servers. Each failed ledger row therefore
//! carries an [`ErrorClass`] and a short reason, and the classes split into
//! two sides: the platform's (it could not serve a valid request) and the
//! caller's (the request could not be served as sent).
//!
//! Rows written before these columns existed have an empty class. They are
//! filed by [`class_for_status`], and [`EFFECTIVE_CLASS_SQL`] applies the same
//! rules in ClickHouse, so old and new rows count alike.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Most characters a recorded reason keeps.
pub const ERROR_REASON_MAX_CHARS: usize = 300;

/// What kind of failure a request met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The request was refused as sent: a bad field, a prompt over the
    /// context window, images to a text-only model.
    ClientInput,
    /// The caller went away before the answer was complete.
    ClientCancel,
    /// The model has been retired.
    Retired,
    /// A rate limit or budget of the caller's tenant or key.
    Limit,
    /// No capacity was free within the admission wait, or the model had no
    /// ready server.
    Capacity,
    /// The model server took too long.
    UpstreamTimeout,
    /// The model server failed: an error status, a crash mid-answer, or a
    /// stream that broke off.
    UpstreamError,
    /// The model server could not be reached.
    Network,
    /// The gateway itself failed.
    Gateway,
}

impl ErrorClass {
    pub const ALL: [ErrorClass; 9] = [
        ErrorClass::ClientInput,
        ErrorClass::ClientCancel,
        ErrorClass::Retired,
        ErrorClass::Limit,
        ErrorClass::Capacity,
        ErrorClass::UpstreamTimeout,
        ErrorClass::UpstreamError,
        ErrorClass::Network,
        ErrorClass::Gateway,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::ClientInput => "client_input",
            ErrorClass::ClientCancel => "client_cancel",
            ErrorClass::Retired => "retired",
            ErrorClass::Limit => "limit",
            ErrorClass::Capacity => "capacity",
            ErrorClass::UpstreamTimeout => "upstream_timeout",
            ErrorClass::UpstreamError => "upstream_error",
            ErrorClass::Network => "network",
            ErrorClass::Gateway => "gateway",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorClass> {
        ErrorClass::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The platform's failure rather than the caller's.
    pub fn is_platform(self) -> bool {
        matches!(
            self,
            ErrorClass::Capacity
                | ErrorClass::UpstreamTimeout
                | ErrorClass::UpstreamError
                | ErrorClass::Network
                | ErrorClass::Gateway
        )
    }
}

/// A failure as the code that set its status saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub class: ErrorClass,
    pub reason: String,
}

impl Failure {
    pub fn new(class: ErrorClass, reason: &str) -> Self {
        Failure {
            class,
            reason: clean_reason(reason),
        }
    }
}

/// The ledger's `(error_class, error_reason)` for a finished request: both
/// empty for a success, else the failure its code recorded, else what the
/// status and admission decision imply.
pub fn ledger_failure(
    status: u16,
    admission: &str,
    failure: Option<&Failure>,
) -> (&'static str, String) {
    if status < 400 {
        return ("", String::new());
    }
    match failure {
        Some(f) if !f.reason.is_empty() => (f.class.as_str(), f.reason.clone()),
        Some(f) => (
            f.class.as_str(),
            reason_for_status(status, admission).to_string(),
        ),
        None => (
            class_for_status(status, admission).map_or("", ErrorClass::as_str),
            reason_for_status(status, admission).to_string(),
        ),
    }
}

/// The class a failed request is filed under when the code that set its
/// status named none, from the status and the admission decision. `None`
/// for a success. Keep in step with [`EFFECTIVE_CLASS_SQL`].
pub fn class_for_status(status: u16, admission: &str) -> Option<ErrorClass> {
    let rejected = admission == "rejected";
    Some(match status {
        0..=399 => return None,
        499 => ErrorClass::ClientCancel,
        410 => ErrorClass::Retired,
        429 => ErrorClass::Limit,
        403 if rejected => ErrorClass::Limit,
        503 if rejected => ErrorClass::Capacity,
        504 => ErrorClass::UpstreamTimeout,
        400..=499 => ErrorClass::ClientInput,
        _ => ErrorClass::UpstreamError,
    })
}

/// A reason for a failure whose code set none, from the same inputs as
/// [`class_for_status`]. Empty for a success.
pub fn reason_for_status(status: u16, admission: &str) -> &'static str {
    let rejected = admission == "rejected";
    match status {
        0..=399 => "",
        499 => "client disconnected",
        410 => "model retired",
        429 => "rate limit or token budget exceeded",
        403 if rejected => "term budget exhausted",
        503 if rejected => "timed out waiting for model capacity",
        504 => "model server timed out",
        400..=499 => "request refused",
        _ => "model server error",
    }
}

/// The class a ledger row is counted under: its own when it has one, else
/// what [`class_for_status`] gives. Empty for a success.
pub const EFFECTIVE_CLASS_SQL: &str = "multiIf(\
    error_class != '', error_class, \
    status_code < 400, '', \
    status_code = 499, 'client_cancel', \
    status_code = 410, 'retired', \
    status_code = 429, 'limit', \
    status_code = 403 AND admission = 'rejected', 'limit', \
    status_code = 503 AND admission = 'rejected', 'capacity', \
    status_code = 504, 'upstream_timeout', \
    status_code < 500, 'client_input', \
    'upstream_error')";

/// The classes [`ErrorClass::is_platform`] holds for, as a SQL list.
pub const PLATFORM_CLASSES_SQL: &str =
    "('capacity', 'upstream_timeout', 'upstream_error', 'network', 'gateway')";

/// The message an error body carries, cut to [`ERROR_REASON_MAX_CHARS`].
///
/// Reads the OpenAI shape (`error.message`, or `error` as a string), the
/// FastAPI shape (`detail` as a string, or a list whose first entry has a
/// `msg`), and a bare `message`. Only those fields are kept: a validation
/// error's `input` echoes the request and is never read. A body that is not
/// JSON gives its first line, unless it looks like HTML.
pub fn reason_from_body(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut message = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => json_message(&v)?,
        Err(_) if text.starts_with('<') => return None,
        Err(_) => text.lines().next().unwrap_or_default().to_string(),
    };
    // A router in front of the model server can wrap the server's whole
    // error body, as a string, in its own error's message.
    for _ in 0..2 {
        match serde_json::from_str::<serde_json::Value>(message.trim()) {
            Ok(inner) => match json_message(&inner) {
                Some(inner) => message = inner,
                None => break,
            },
            Err(_) => break,
        }
    }
    let reason = clean_reason(&message);
    (!reason.is_empty()).then_some(reason)
}

fn json_message(v: &serde_json::Value) -> Option<String> {
    let str_at = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    if let Some(err) = v.get("error") {
        if let Some(s) = err.as_str() {
            return Some(s.to_string());
        }
        if let Some(s) = str_at(err, "message") {
            return Some(s);
        }
    }
    match v.get("detail") {
        Some(serde_json::Value::String(s)) => return Some(s.clone()),
        Some(serde_json::Value::Array(items)) => {
            if let Some(msg) = items.first().and_then(|i| str_at(i, "msg")) {
                return Some(msg);
            }
        }
        _ => {}
    }
    str_at(v, "message")
}

/// One line, runs of whitespace collapsed, cut to the cap.
pub fn clean_reason(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(ERROR_REASON_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &collapsed[..cut]),
        None => collapsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_file_under_the_side_that_failed() {
        assert_eq!(class_for_status(200, "fast"), None);
        assert_eq!(class_for_status(400, "fast"), Some(ErrorClass::ClientInput));
        assert_eq!(class_for_status(413, "fast"), Some(ErrorClass::ClientInput));
        assert_eq!(
            class_for_status(499, "queued"),
            Some(ErrorClass::ClientCancel)
        );
        assert_eq!(class_for_status(410, "rejected"), Some(ErrorClass::Retired));
        assert_eq!(class_for_status(429, "rejected"), Some(ErrorClass::Limit));
        assert_eq!(class_for_status(403, "rejected"), Some(ErrorClass::Limit));
        assert_eq!(class_for_status(403, "fast"), Some(ErrorClass::ClientInput));
        assert_eq!(
            class_for_status(503, "rejected"),
            Some(ErrorClass::Capacity)
        );
        assert_eq!(
            class_for_status(503, "fast"),
            Some(ErrorClass::UpstreamError)
        );
        assert_eq!(
            class_for_status(504, "fast"),
            Some(ErrorClass::UpstreamTimeout)
        );
        assert_eq!(
            class_for_status(502, "fast"),
            Some(ErrorClass::UpstreamError)
        );
        assert_eq!(
            class_for_status(500, "queued"),
            Some(ErrorClass::UpstreamError)
        );
    }

    #[test]
    fn the_ledger_keeps_a_recorded_failure_and_files_the_rest_by_status() {
        assert_eq!(ledger_failure(200, "fast", None), ("", String::new()));
        let cut = Failure::new(ErrorClass::UpstreamError, "stream broke off");
        assert_eq!(ledger_failure(200, "fast", Some(&cut)), ("", String::new()));
        assert_eq!(
            ledger_failure(502, "fast", Some(&cut)),
            ("upstream_error", "stream broke off".to_string())
        );
        assert_eq!(
            ledger_failure(503, "rejected", None),
            (
                "capacity",
                "timed out waiting for model capacity".to_string()
            )
        );
        let blank = Failure::new(ErrorClass::Network, "  ");
        assert_eq!(
            ledger_failure(502, "fast", Some(&blank)),
            ("network", "model server error".to_string())
        );
    }

    #[test]
    fn the_sql_names_every_class_the_rust_rules_give() {
        // The two rule sets cannot run side by side here; at least every
        // class the fallback produces must be spelled in the SQL.
        for status in 400..600u16 {
            for admission in ["fast", "queued", "rejected"] {
                let class = class_for_status(status, admission).unwrap();
                assert!(
                    EFFECTIVE_CLASS_SQL.contains(&format!("'{}'", class.as_str())),
                    "{status}/{admission}"
                );
            }
        }
        for class in ErrorClass::ALL {
            assert_eq!(
                PLATFORM_CLASSES_SQL.contains(&format!("'{}'", class.as_str())),
                class.is_platform(),
                "{class:?}"
            );
            assert_eq!(ErrorClass::parse(class.as_str()), Some(class));
        }
    }

    #[test]
    fn reasons_come_from_the_error_shapes_servers_send() {
        let openai = br#"{"error":{"message":"This model's maximum context length is 262144 tokens.","type":"BadRequestError","code":400}}"#;
        assert_eq!(
            reason_from_body(openai).as_deref(),
            Some("This model's maximum context length is 262144 tokens.")
        );
        assert_eq!(
            reason_from_body(br#"{"detail":"Not Found"}"#).as_deref(),
            Some("Not Found")
        );
        let pydantic = br#"{"detail":[{"type":"missing","loc":["body","messages"],"msg":"Field required","input":{"secret":"prompt text"}}]}"#;
        assert_eq!(
            reason_from_body(pydantic).as_deref(),
            Some("Field required")
        );
        assert_eq!(
            reason_from_body(br#"{"error":"no healthy upstream"}"#).as_deref(),
            Some("no healthy upstream")
        );
        assert_eq!(
            reason_from_body(b"upstream connect error or disconnect/reset before headers\nmore")
                .as_deref(),
            Some("upstream connect error or disconnect/reset before headers")
        );
        let wrapped = br#"{"error":{"code":null,"param":null,"message":"{\"error\":{\"message\":\"deepseek-v4-1-flash is not a multimodal model\",\"type\":\"BadRequestError\",\"code\":400}}","type":"invalid_request_error"}}"#;
        assert_eq!(
            reason_from_body(wrapped).as_deref(),
            Some("deepseek-v4-1-flash is not a multimodal model")
        );
        assert_eq!(reason_from_body(b"<html><body>502</body></html>"), None);
        assert_eq!(reason_from_body(b"  "), None);
        assert_eq!(reason_from_body(br#"{"ok":true}"#), None);
    }

    #[test]
    fn a_long_reason_is_cut_on_a_character_boundary() {
        let long = "é".repeat(ERROR_REASON_MAX_CHARS + 50);
        let cut = clean_reason(&long);
        assert_eq!(cut.chars().count(), ERROR_REASON_MAX_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(clean_reason("a\n  b\tc"), "a b c");
    }
}
