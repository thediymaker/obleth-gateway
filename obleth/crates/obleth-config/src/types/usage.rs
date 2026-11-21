//! The usage ledger row and key rejections.

use super::*;

/// A single completed request's telemetry record (the ClickHouse ledger row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub model: String,
    pub admission: String,
    pub weight: i64,
    /// Every prompt token, cached ones included.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// The part of `input_tokens` the upstream reported as served from its
    /// prefix cache (`prompt_tokens_details.cached_tokens`); 0 when it reported
    /// none or the gateway answered without an upstream call. Unrelated to
    /// `cache_status`, the gateway's own response cache. `#[serde(default)]`
    /// keeps older WAL records replayable.
    #[serde(default)]
    pub cached_input_tokens: u32,
    pub estimated_tokens: u32,
    pub queue_wait_ms: u32,
    pub ttft_ms: u32,
    pub total_ms: u32,
    pub status_code: u16,
    /// Response cache outcome: `hit`, `miss`, or `off` (caching not enabled).
    pub cache_status: String,
    /// USD cost of this request, frozen at completion using the model's
    /// per-token (and per-modality) rates in effect at that moment. Stored so
    /// spend reporting never has to recompute from tokens × current price, and
    /// so editing a model's price later can't rewrite historical spend.
    /// `#[serde(default)]` keeps WAL records written before this field existed
    /// replayable.
    #[serde(default)]
    pub cost_usd: f64,
    /// Energy attributed to this request in watt-hours, frozen at completion
    /// from the live cluster power reading and the model's declared
    /// `energy_slots_per_node` — like `cost_usd`, never recomputed later.
    /// Charged on serving time only (`total_ms - queue_wait_ms`).
    #[serde(default)]
    pub energy_wh: f64,
    /// USD cost of `energy_wh` at the electricity rate in effect at completion.
    #[serde(default)]
    pub energy_cost_usd: f64,
    /// Grams of CO2 for `energy_wh` at the grid intensity in effect at completion.
    #[serde(default)]
    pub co2_g: f64,
    /// Unix epoch milliseconds at request completion.
    pub ts_ms: i64,
    /// Client-supplied session/conversation id used to group related requests
    /// in the live request log (e.g. a multi-turn chat). Empty when the caller
    /// did not provide one. `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub session_id: String,
    /// How the session_id was obtained: `client` (caller supplied it),
    /// `derived` (proxy generated it from request fingerprint), or `none`
    /// (no session grouping). `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub session_id_source: String,
    /// Coarse request class derived from the request path (e.g. `chat`,
    /// `embedding`, `audio`, `image`, `completion`, `responses`, `rerank`,
    /// `other`). Surfaced as the "Type" column in the request log.
    /// `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub request_type: String,
    /// Device identifier carried by the bearer token (identity-key requests
    /// only; empty for secret keys). Sanitised before it gets here
    /// (`sanitize_device_id`). `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub device_id: String,
    /// The end user a shared front end named for this request, on keys with
    /// `end_user_fairshare` on (`x-obleth-end-user` header or the body's
    /// `user`); empty otherwise. `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub end_user: String,
    /// On a helper-call row (a boon's call to another model: a speculation
    /// draft, an image description, ...), the `request_id` of the client
    /// request it served; nil on every other row. `#[serde(default)]` keeps
    /// older WAL records replayable as "none".
    #[serde(default)]
    pub parent_request_id: Uuid,
    /// The variant name the client asked for when it called the model through
    /// one ([`ModelVariant`]); empty otherwise. `model` stays the parent
    /// model's name either way, so per-model reports and pools are unchanged.
    /// `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub model_variant: String,
    /// The reasoning effort the request was served at (`none`, `low`,
    /// `medium`, `high`), `unsupported` when the model has no effort control
    /// and the caller's was removed, or empty when the effort went through as
    /// sent (or none was named). `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub reasoning_effort: String,
    /// The model family the client called by name ([`crate::ModelFamily`]);
    /// empty otherwise. `model` is the member that served the request.
    /// `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub model_family: String,
    /// The reasoning effort the caller asked for, folded to the four levels
    /// when it was one obleth knows; empty when none was named.
    #[serde(default)]
    pub reasoning_effort_requested: String,
    /// The key's and tenant's names as they were when the request was served.
    /// Frozen on the row like `cost_usd`, so usage stays attributable after the
    /// key or tenant is deleted or renamed, for as long as the row is kept.
    /// `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub key_name: String,
    #[serde(default)]
    pub tenant_name: String,
    /// Whether the reply reached the client as the upstream produced it (an
    /// event stream). `ttft_ms` is measured only then: a buffered reply's
    /// first byte is its last, so it records 0 there. `#[serde(default)]`
    /// keeps older WAL records replayable.
    #[serde(default)]
    pub streamed: bool,
    /// What kind of failure the request met (`ErrorClass::as_str`), or empty
    /// for a success. `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub error_class: String,
    /// Why it failed, in the words of whoever refused it (the model server's
    /// error message, or the gateway's own), cut to `ERROR_REASON_MAX_CHARS`.
    /// Never request or answer text. Empty for a success.
    #[serde(default)]
    pub error_reason: String,
    /// The address the request came from (see `obleth_config::network`):
    /// the rightmost `X-Forwarded-For` entry when the peer is a trusted
    /// proxy, else the peer. Empty when unknown (in-process callers, helper
    /// rows). Expires after the client address retention, while the row
    /// stays. `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub client_ip: String,
    /// The network `client_ip` is grouped under (IPv4 /24, IPv6 /48), kept
    /// beside it so reads need no address arithmetic; expires with it.
    #[serde(default)]
    pub client_network: String,
    /// Two-letter country the edge named for the client, only when the peer
    /// is a trusted proxy and the client address is public; empty otherwise.
    /// Kept as long as the row.
    #[serde(default)]
    pub client_country: String,
    /// The client's User-Agent, cut to 256 characters. Expires with
    /// `client_ip`.
    #[serde(default)]
    pub user_agent: String,
}

/// Why a known key was refused before its request ran, as recorded in the
/// `key_rejections` ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyRejectionReason {
    /// The key is disabled.
    Disabled,
    /// The request came from outside the key's `allowed_networks` and
    /// `allowed_countries`.
    IpNotAllowed,
}

impl KeyRejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyRejectionReason::Disabled => "disabled",
            KeyRejectionReason::IpNotAllowed => "ip_not_allowed",
        }
    }
}

/// One refused use of a real key. The ledger keeps at most one row per key,
/// address and reason in each short window, with `count` saying how many
/// refusals it stands for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRejection {
    /// Unix epoch milliseconds of the first refusal the row stands for.
    pub ts_ms: i64,
    pub key_id: Uuid,
    pub key_name: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub reason: KeyRejectionReason,
    pub client_ip: String,
    pub client_country: String,
    pub user_agent: String,
    /// The request path, without its query string.
    pub path: String,
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_record_energy_fields_default_for_old_wal() {
        // A record serialized before the energy fields existed must replay.
        let old = r#"{"request_id":"00000000-0000-0000-0000-000000000000",
            "tenant_id":"00000000-0000-0000-0000-000000000000",
            "key_id":"00000000-0000-0000-0000-000000000000",
            "model":"m","admission":"fast","weight":1,"input_tokens":1,
            "output_tokens":1,"estimated_tokens":2,"queue_wait_ms":0,"ttft_ms":0,
            "total_ms":10,"status_code":200,"cache_status":"off","ts_ms":0}"#;
        let r: UsageRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.energy_wh, 0.0);
        assert_eq!(r.energy_cost_usd, 0.0);
        assert_eq!(r.co2_g, 0.0);
    }

    #[test]
    fn usage_record_cached_input_tokens_default_for_old_wal() {
        // A record serialized before cached prompt tokens were recorded must
        // replay as "none cached", and a new one must round-trip the count.
        let old = r#"{"request_id":"00000000-0000-0000-0000-000000000000",
            "tenant_id":"00000000-0000-0000-0000-000000000000",
            "key_id":"00000000-0000-0000-0000-000000000000",
            "model":"m","admission":"fast","weight":1,"input_tokens":100,
            "output_tokens":1,"estimated_tokens":2,"queue_wait_ms":0,"ttft_ms":0,
            "total_ms":10,"status_code":200,"cache_status":"off","cost_usd":0.5,
            "ts_ms":0,"session_id":"s","request_type":"chat"}"#;
        let mut r: UsageRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.cached_input_tokens, 0);
        assert_eq!(r.input_tokens, 100);
        r.cached_input_tokens = 80;
        let back: UsageRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.cached_input_tokens, 80);
    }
}
