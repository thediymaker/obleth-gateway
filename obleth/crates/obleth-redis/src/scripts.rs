//! Atomic token-bucket Lua scripts.
//!
//! Budget enforcement must be atomic across many gateway pods, so the
//! check-refill-reserve sequence runs server-side in Redis. Fairness is measured
//! in tokens: we *reserve* an estimate at admission and *reconcile* the true cost
//! after the stream completes.

/// Reserve `requested` tokens from a tenant's bucket.
///
/// KEYS[1] = bucket key
/// ARGV[1] = capacity (burst ceiling, tokens)
/// ARGV[2] = refill_per_ms (tokens added per millisecond)
/// ARGV[3] = now_ms
/// ARGV[4] = requested tokens
///
/// Returns `{allowed (0|1), remaining_tokens}`.
pub const RESERVE: &str = r#"
local key   = KEYS[1]
local cap   = tonumber(ARGV[1])
local rate  = tonumber(ARGV[2])
local now   = tonumber(ARGV[3])
local req   = tonumber(ARGV[4])

local data    = redis.call('HMGET', key, 'tokens', 'ts')
local tokens  = tonumber(data[1])
local ts      = tonumber(data[2])
if tokens == nil then tokens = cap end
if ts == nil then ts = now end

local elapsed = now - ts
if elapsed < 0 then elapsed = 0 end
tokens = math.min(cap, tokens + elapsed * rate)

local allowed = 0
if tokens >= req then
  tokens = tokens - req
  allowed = 1
end

redis.call('HSET', key, 'tokens', tokens, 'ts', now)
redis.call('PEXPIRE', key, 600000)
return { allowed, math.floor(tokens) }
"#;

/// Reconcile estimated vs actual cost after completion.
///
/// KEYS[1] = bucket key
/// ARGV[1] = capacity
/// ARGV[2] = delta (estimated - actual; positive refunds, negative charges more)
///
/// Returns remaining tokens.
pub const RECONCILE: &str = r#"
local key   = KEYS[1]
local cap   = tonumber(ARGV[1])
local delta = tonumber(ARGV[2])

local tokens = tonumber(redis.call('HGET', key, 'tokens'))
if tokens == nil then tokens = cap end

tokens = math.min(cap, tokens + delta)
-- allow a bounded debt so over-budget requests are paid back over time
if tokens < -cap then tokens = -cap end

redis.call('HSET', key, 'tokens', tokens)
return math.floor(tokens)
"#;

/// Read a tenant's cumulative term usage, rolling the period if it changed.
///
/// KEYS[1] = term-usage key
/// ARGV[1] = period_key (opaque string identifying the current term/month)
///
/// If the stored period differs from the supplied one, the counters reset to
/// zero before the read. Returns `{tokens, cost_string}`.
pub const TERM_USAGE_READ: &str = r#"
local key    = KEYS[1]
local period = ARGV[1]

local stored = redis.call('HGET', key, 'period')
if stored ~= period then
  redis.call('DEL', key)
  redis.call('HSET', key, 'period', period)
end
local tokens = tonumber(redis.call('HGET', key, 'tokens')) or 0
local cost   = redis.call('HGET', key, 'cost') or '0'
return { tokens, cost }
"#;

/// Add observed usage to a tenant's cumulative term counters, rolling first.
///
/// KEYS[1] = term-usage key
/// ARGV[1] = period_key
/// ARGV[2] = add_tokens
/// ARGV[3] = add_cost (USD)
///
/// Returns `{tokens_after, cost_after_string}`.
pub const TERM_USAGE_ADD: &str = r#"
local key    = KEYS[1]
local period = ARGV[1]

local stored = redis.call('HGET', key, 'period')
if stored ~= period then
  redis.call('DEL', key)
  redis.call('HSET', key, 'period', period)
end
local tokens = redis.call('HINCRBY', key, 'tokens', tonumber(ARGV[2]))
local cost   = redis.call('HINCRBYFLOAT', key, 'cost', tonumber(ARGV[3]))
-- safety expiry so abandoned tenants don't linger forever (refreshed on use)
redis.call('PEXPIRE', key, 31536000000)
return { tokens, cost }
"#;
