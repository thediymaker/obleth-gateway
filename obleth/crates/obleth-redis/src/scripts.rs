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
/// An estimate larger than the whole bucket can never fit, so it is admitted
/// when the bucket is full and runs into the bounded debt (floor `-capacity`)
/// instead of being rejected forever.
///
/// Returns `{allowed (0|1), remaining_tokens}`.
pub const RESERVE: &str = r#"
local key   = KEYS[1]
local cap   = tonumber(ARGV[1])
local rate  = tonumber(ARGV[2])
local now   = tonumber(ARGV[3])
local req   = tonumber(ARGV[4])

if cap <= 0 then
  return { 1, 0 }
end

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
elseif req > cap and tokens >= cap then
  tokens = math.max(tokens - req, -cap)
  allowed = 1
end

redis.call('HSET', key, 'tokens', tokens, 'ts', now)
redis.call('PEXPIRE', key, 600000)
return { allowed, math.floor(tokens) }
"#;

/// Combined admission check: cumulative term budget gate + token-bucket
/// reserve, in one round trip.
///
/// The term gate runs FIRST so a term-exhausted request never reserves bucket
/// tokens it would have no way to refund (the response stream — where
/// reconciliation happens — never runs for rejected requests).
///
/// The gate counts both committed usage (term-usage hash `tokens`/`cost`) and
/// in-flight reservations (term-reserved hash `tokens`/`cost`), rejecting when
/// `committed + reserved >= cap`. With `reserve_term = 1` an admitted request
/// adds its estimate to the reserved hash in the same script, so N concurrent
/// requests at cap-1 cannot all pass. The reservation is only made when the
/// bucket also admits, since rejected requests are never settled.
///
/// Why reservations live in their own short-lived key: a request that is
/// admitted but never settled (crash, SIGKILL, missed release) would otherwise
/// hold its reservation for the life of a lifetime/term period and eventually
/// block the scope. The reserved hash gets a 10-minute TTL when it is created
/// and is never re-armed (refreshing on every reserve would keep a leak alive
/// forever in a scope that is never idle), so every leak is bounded to ~10
/// minutes in all scopes. The cost: when the key expires, all in-flight
/// reservations vanish at once and the next reserve starts a fresh key, so
/// over-admission is briefly possible (bounded by concurrency x estimate).
/// Commits at settle time are always exact.
///
/// KEYS[1] = bucket key
/// KEYS[2] = term-usage key (committed usage; `period`, `tokens`, `cost`)
/// KEYS[3] = term-reserved key (in-flight; `period`, `tokens`, `cost`)
/// ARGV[1] = capacity (burst ceiling, tokens)
/// ARGV[2] = refill_per_ms
/// ARGV[3] = now_ms
/// ARGV[4] = requested tokens (also the term token reservation)
/// ARGV[5] = check_term (0|1)
/// ARGV[6] = period_key (ignored when check_term is 0)
/// ARGV[7] = term token cap ('' = uncapped)
/// ARGV[8] = term cost cap in USD ('' = uncapped)
/// ARGV[9] = reserve_term (0|1; ignored when check_term is 0)
/// ARGV[10] = estimated cost in USD to reserve
///
/// Returns `{status, remaining_tokens, term_tokens, term_cost_string}` where
/// status is 1 (reserved), 0 (per-minute bucket exhausted), or -1 (term budget
/// exhausted; nothing reserved). `term_tokens`/`term_cost` are the committed
/// usage observed by the gate.
pub const RESERVE_WITH_TERM: &str = r#"
local bucket = KEYS[1]
local term   = KEYS[2]
local rkey   = KEYS[3]
local cap    = tonumber(ARGV[1])
local rate   = tonumber(ARGV[2])
local now    = tonumber(ARGV[3])
local req    = tonumber(ARGV[4])
local check_term = ARGV[5] == '1'
local period = ARGV[6]

local term_tokens = 0
local term_cost   = '0'
if check_term then
  local stored = redis.call('HGET', term, 'period')
  if stored ~= period then
    redis.call('DEL', term)
    redis.call('HSET', term, 'period', period)
    redis.call('PEXPIRE', term, 31536000000)
  end
  local t = redis.call('HMGET', term, 'tokens', 'cost')
  term_tokens = tonumber(t[1]) or 0
  term_cost   = t[2] or '0'

  -- missing/expired reserved key reads as 0; a stale period is discarded
  local r = redis.call('HMGET', rkey, 'period', 'tokens', 'cost')
  local reserved_tokens = 0
  local reserved_cost   = 0
  if r[1] == period then
    reserved_tokens = tonumber(r[2]) or 0
    reserved_cost   = tonumber(r[3]) or 0
  elseif r[1] then
    redis.call('DEL', rkey)
  end
  local cap_tokens = ARGV[7]
  local cap_cost   = ARGV[8]
  if (cap_tokens ~= '' and term_tokens + reserved_tokens >= tonumber(cap_tokens))
     or (cap_cost ~= '' and tonumber(term_cost) + reserved_cost >= tonumber(cap_cost)) then
    return { -1, 0, term_tokens, term_cost }
  end
end

local allowed   = 1
local remaining = 0
if cap > 0 then
  local data    = redis.call('HMGET', bucket, 'tokens', 'ts')
  local tokens  = tonumber(data[1])
  local ts      = tonumber(data[2])
  if tokens == nil then tokens = cap end
  if ts == nil then ts = now end

  local elapsed = now - ts
  if elapsed < 0 then elapsed = 0 end
  tokens = math.min(cap, tokens + elapsed * rate)

  allowed = 0
  if tokens >= req then
    tokens = tokens - req
    allowed = 1
  elseif req > cap and tokens >= cap then
    -- see RESERVE: an estimate above capacity may only drain a full bucket
    tokens = math.max(tokens - req, -cap)
    allowed = 1
  end

  redis.call('HSET', bucket, 'tokens', tokens, 'ts', now)
  redis.call('PEXPIRE', bucket, 600000)
  remaining = math.floor(tokens)
end

if allowed == 1 and check_term and ARGV[9] == '1' then
  redis.call('HSET', rkey, 'period', period)
  redis.call('HINCRBY', rkey, 'tokens', req)
  redis.call('HINCRBYFLOAT', rkey, 'cost', ARGV[10])
  -- arm the TTL only on creation (PTTL -1); never refresh, see above.
  -- Emulates PEXPIRE ... NX so Redis 6.x deployments keep working.
  if redis.call('PTTL', rkey) < 0 then redis.call('PEXPIRE', rkey, 600000) end
end

return { allowed, remaining, term_tokens, term_cost }
"#;

/// Reconcile estimated vs actual cost after completion.
///
/// KEYS[1] = bucket key
/// ARGV[1] = capacity
/// ARGV[2] = delta (estimated - actual; positive refunds, negative charges more)
/// ARGV[3] = now_ms
///
/// Returns remaining tokens.
pub const RECONCILE: &str = r#"
local key   = KEYS[1]
local cap   = tonumber(ARGV[1])
local delta = tonumber(ARGV[2])
local now   = tonumber(ARGV[3])

if cap <= 0 then
  return 0
end

local tokens = tonumber(redis.call('HGET', key, 'tokens'))
local recreated = tokens == nil
if recreated then tokens = cap end

tokens = math.min(cap, tokens + delta)
-- allow a bounded debt so over-budget requests are paid back over time
if tokens < -cap then tokens = -cap end

if recreated then
  -- the bucket expired mid-request; without ts the next reserve would treat
  -- the debt as fresh and without a TTL the hash would never expire
  redis.call('HSET', key, 'tokens', tokens, 'ts', now)
else
  redis.call('HSET', key, 'tokens', tokens)
end
redis.call('PEXPIRE', key, 600000)
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

/// Settle a request admitted by `RESERVE_WITH_TERM` with `reserve_term = 1`:
/// release its reservation and commit its actual usage, atomically.
///
/// KEYS[1] = term-usage key (committed)
/// KEYS[2] = term-reserved key (in-flight, see `RESERVE_WITH_TERM`)
/// ARGV[1] = period_key the reservation was made under
/// ARGV[2] = estimated tokens (reservation to release)
/// ARGV[3] = estimated cost USD (reservation to release)
/// ARGV[4] = actual tokens
/// ARGV[5] = actual cost USD
///
/// Reservations are floored at 0 so an over-release can never create
/// headroom. The release is skipped when the reserved key has expired or
/// belongs to another period (the reservation is already gone); the commit
/// always happens. If the committed counters have since rolled to another
/// period, the actual usage is counted against the current period (rolling
/// back here would wipe the newer period's usage). The reserved key's TTL is
/// never refreshed here (or anywhere after creation).
///
/// Returns `{tokens_after, cost_after_string}`.
pub const TERM_RECONCILE: &str = r#"
local key    = KEYS[1]
local rkey   = KEYS[2]
local period = ARGV[1]

if redis.call('HGET', rkey, 'period') == period then
  local rt = redis.call('HINCRBY', rkey, 'tokens', -tonumber(ARGV[2]))
  if rt < 0 then redis.call('HSET', rkey, 'tokens', 0) end
  local rc = tonumber(redis.call('HINCRBYFLOAT', rkey, 'cost', -tonumber(ARGV[3])))
  -- snap float residue (e.g. 1e-17 after 0.1 + 0.2 - 0.3) to exactly zero
  if rc < 1e-12 then redis.call('HSET', rkey, 'cost', '0') end
end

if not redis.call('HGET', key, 'period') then
  redis.call('HSET', key, 'period', period)
end

local tokens = redis.call('HINCRBY', key, 'tokens', tonumber(ARGV[4]))
local cost   = redis.call('HINCRBYFLOAT', key, 'cost', tonumber(ARGV[5]))
redis.call('PEXPIRE', key, 31536000000)
return { tokens, cost }
"#;
