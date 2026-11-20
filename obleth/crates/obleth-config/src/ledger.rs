//! SQL over the usage ledger's columns, shared by the daily rollup the
//! telemetry writer keeps and the reads the admin API serves, so both count
//! a request the same way.

/// A row's first-token time marks a real first token. Streamed rows always
/// qualify. Rows written before v2.3.0 (2026-10-20) carry a first-byte time
/// even for buffered replies, whose first byte arrives with the whole body,
/// so the rest of the reply takes a few milliseconds. Their time counts only
/// when at least a twentieth of the request came after it, as it does for a
/// streamed reply. A buffered reply recorded since has no first-token time.
///
/// TODO(2027-04-18): those rows pass the default 180-day retention then;
/// once every deployment's ledger has no row from before v2.3.0, drop this
/// clause from [`TTFT_OK_SQL`] and [`DECODE_MS_SQL`].
macro_rules! real_ttft {
    () => {
        "(streamed or (total_ms - ttft_ms) * 20 >= total_ms - queue_wait_ms)"
    };
}

/// The requests a first-token time is averaged over: answered (2xx/3xx), not
/// from the response cache, with a real first token (see `real_ttft`).
/// Every first-token figure, raw or from the daily rollup, counts these and
/// only these, so the Overview, model pages and Reports agree.
pub const TTFT_OK_SQL: &str = concat!(
    "(ttft_ms > 0 and status_code >= 200 and status_code < 400 \
     and cache_status != 'hit' and ",
    real_ttft!(),
    ")"
);

/// Milliseconds a reply spent generating: the decode window (total less first
/// token) when the first token is real, else the upstream time (total less
/// queue). Measuring a buffered reply from its first byte would leave only the
/// body's transfer time and a speed in the tens of thousands.
pub const DECODE_MS_SQL: &str = concat!(
    "if(ttft_ms > 0 and total_ms - ttft_ms >= 20 and ",
    real_ttft!(),
    ", total_ms - ttft_ms, total_ms - queue_wait_ms)"
);
