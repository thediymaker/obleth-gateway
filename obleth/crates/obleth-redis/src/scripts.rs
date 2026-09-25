//! Atomic Lua scripts: token buckets, replica heartbeats and shared fairshare
//! slots.
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
  end
  -- clears a TTL left by an earlier version (committed usage never expires)
  redis.call('PERSIST', term)
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
-- clears a TTL left by an earlier version (committed usage never expires)
redis.call('PERSIST', key)
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
-- no TTL: committed usage is budget state, not cache. Under a volatile-*
-- eviction policy a TTL would make it evictable, silently resetting the
-- scope's spend mid-period. PERSIST also clears the 1-year TTL that
-- earlier versions set.
redis.call('PERSIST', key)
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
redis.call('PERSIST', key)
return { tokens, cost }
"#;

// ---------------------------------------------------------------------------
// Shared fairshare slots
//
// Cluster-wide admission slots live in two kinds of hash, both without a TTL
// so volatile-lru never evicts them:
//
// - the totals hash (`obleth:fairshare:slots`): the cluster-wide count per
//   limit, `g` for every pool together, `p|<pool>` per pool, and
//   `t|<pool>|<tenant>` and `k|<pool>|<key>` per tenant and key in a pool,
//   plus wait markers (`w|<pool>`, and `w` for the global ceiling) holding
//   the Redis time until which some replica is waiting for a slot there;
// - one holdings hash per replica (`obleth:fairshare:held:<instance>`) with
//   the same count fields for what that replica holds.
//
// Every change to a holdings hash is applied to the totals in the same
// script, so the totals are always the sum of the holdings. A replica's
// holdings are reclaimed when its heartbeat expires from the replica set, by
// whichever live replica heartbeats next. A release only frees what its
// replica's holdings record, so a release that arrives after a reclaim is a
// no-op instead of freeing someone else's slot. Counts at zero are deleted.
//
// Releases, reconciles and reclaims publish the freed pool's id on the
// release channel, but only while a wait marker for that pool (or the global
// ceiling) is current, so an uncontended fleet publishes nothing.
// ---------------------------------------------------------------------------

/// Lua helpers shared by the slot scripts: the Redis clock in ms, a counter
/// adjustment that deletes the field at zero, the release wakeup, and the
/// reclaim of one replica's holdings.
macro_rules! slot_helpers {
    () => {
        r#"
if redis.replicate_commands then redis.replicate_commands() end
local function now_ms()
  local t = redis.call('TIME')
  return tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
end
local function adjust(key, field, n)
  local v = redis.call('HINCRBY', key, field, n)
  if v <= 0 then redis.call('HDEL', key, field) end
  return v
end
local function wake(totals, pool, channel, now)
  local w = redis.call('HMGET', totals, 'w|' .. pool, 'w')
  local wp, wg = tonumber(w[1]) or 0, tonumber(w[2]) or 0
  if wp > now or wg > now then
    redis.call('PUBLISH', channel, pool)
  end
  if wp ~= 0 and wp <= now then redis.call('HDEL', totals, 'w|' .. pool) end
  if wg ~= 0 and wg <= now then redis.call('HDEL', totals, 'w') end
end
local function reclaim(totals, held, channel, now)
  local h = redis.call('HGETALL', held)
  for i = 1, #h, 2 do
    local n = tonumber(h[i + 1]) or 0
    if n > 0 then
      adjust(totals, h[i], -n)
      if string.sub(h[i], 1, 2) == 'p|' then
        wake(totals, string.sub(h[i], 3), channel, now)
      end
    end
  end
  redis.call('DEL', held)
end
"#
    };
}

/// Register one gateway replica's heartbeat, reclaim the slot holdings of
/// every replica whose heartbeat has expired, and count the live replicas, in
/// one round trip.
///
/// KEYS[1] = replica set (sorted set: member = instance id, score = expiry ms)
/// KEYS[2] = slot totals hash
/// ARGV[1] = instance id
/// ARGV[2] = heartbeat TTL in ms
/// ARGV[3] = holdings hash prefix (the instance id is appended)
/// ARGV[4] = release channel
///
/// Expiry is scored on the Redis clock (`TIME`), not the caller's, so skew
/// between pods cannot keep a dead replica counted or drop a live one. Expired
/// members are pruned on every call, so a crashed replica stops being counted
/// one TTL after its last heartbeat, and its slots return to the fleet at the
/// same moment. A live replica's holdings are never touched. The set itself
/// carries no TTL: it is one member per live replica, and under the chart's
/// volatile-lru policy a TTL would make it evictable, briefly collapsing
/// every replica's count to 1.
///
/// The holdings keys are derived from the member names inside the script,
/// which a standalone Redis (the only topology the gateway supports) allows.
///
/// Returns the live replica count, this one included.
pub const REPLICA_HEARTBEAT: &str = concat!(
    slot_helpers!(),
    r#"
local key = KEYS[1]
local ttl = tonumber(ARGV[2])
local now = now_ms()

redis.call('ZADD', key, now + ttl, ARGV[1])
local expired = redis.call('ZRANGEBYSCORE', key, '-inf', now)
for _, instance in ipairs(expired) do
  reclaim(KEYS[2], ARGV[3] .. instance, ARGV[4], now)
end
redis.call('ZREMRANGEBYSCORE', key, '-inf', now)
return redis.call('ZCARD', key)
"#
);

/// Deregister a replica on clean shutdown: drop it from the replica set and
/// free anything its holdings still record (normally nothing, once drained).
///
/// KEYS[1] = replica set, KEYS[2] = slot totals hash, KEYS[3] = its holdings
/// ARGV[1] = instance id, ARGV[2] = release channel
pub const REPLICA_DEREGISTER: &str = concat!(
    slot_helpers!(),
    r#"
reclaim(KEYS[2], KEYS[3], ARGV[2], now_ms())
redis.call('ZREM', KEYS[1], ARGV[1])
return 1
"#
);

/// Take one cluster-wide slot, checked against every limit at once.
///
/// KEYS[1] = slot totals hash, KEYS[2] = this replica's holdings,
/// KEYS[3] = replica set
/// ARGV[1] = instance id, ARGV[2] = pool id, ARGV[3] = tenant, ARGV[4] = key
/// ARGV[5] = pool cap, ARGV[6] = global cap,
/// ARGV[7] = tenant cap (0: none), ARGV[8] = key cap (0: none)
/// ARGV[9] = how long a refusal marks the pool as waited on, in ms
///
/// A replica whose heartbeat has expired may not take slots: its holdings
/// are about to be (or have been) reclaimed. Otherwise, if the pool, the
/// global count, the tenant and the key are all under their caps, all four
/// counters are taken together, in the totals and in the holdings; if any is
/// at its cap nothing is taken and the pool (or, for the global ceiling, the
/// ceiling) is marked as waited on, so the next release there publishes.
///
/// Returns `{code, pool count, global count}`, counts after the call. Codes:
/// 1 granted, 0 pool full, -1 global ceiling full, -2 tenant cap, -3 key cap,
/// -4 not live.
pub const SLOT_ACQUIRE: &str = concat!(
    slot_helpers!(),
    r#"
local totals, held = KEYS[1], KEYS[2]
local pool = ARGV[2]
local pf = 'p|' .. pool
local tf = 't|' .. pool .. '|' .. ARGV[3]
local kf = 'k|' .. pool .. '|' .. ARGV[4]
local now = now_ms()
local c = redis.call('HMGET', totals, 'g', pf, tf, kf)
local g, p = tonumber(c[1]) or 0, tonumber(c[2]) or 0
local tn, kn = tonumber(c[3]) or 0, tonumber(c[4]) or 0

local live = tonumber(redis.call('ZSCORE', KEYS[3], ARGV[1]))
if live == nil or live <= now then
  return { -4, p, g }
end

local tcap, kcap = tonumber(ARGV[7]), tonumber(ARGV[8])
local code = 1
if p >= tonumber(ARGV[5]) then code = 0
elseif g >= tonumber(ARGV[6]) then code = -1
elseif tcap > 0 and tn >= tcap then code = -2
elseif kcap > 0 and kn >= kcap then code = -3
end
if code ~= 1 then
  local mark = 'w|' .. pool
  if code == -1 then mark = 'w' end
  redis.call('HSET', totals, mark, now + tonumber(ARGV[9]))
  return { code, p, g }
end

for _, f in ipairs({ 'g', pf, tf, kf }) do
  redis.call('HINCRBY', totals, f, 1)
  redis.call('HINCRBY', held, f, 1)
end
return { 1, p + 1, g + 1 }
"#
);

/// Give back one slot this replica holds.
///
/// KEYS[1] = slot totals hash, KEYS[2] = this replica's holdings
/// ARGV[1] = pool id, ARGV[2] = tenant, ARGV[3] = key, ARGV[4] = release
/// channel
///
/// Each counter is decremented only where this replica's holdings still
/// record it, so a release after a reclaim or a reconcile that already
/// dropped the slot frees nothing twice. Publishes the pool id when a slot
/// was freed and someone is waiting on the pool or the ceiling.
///
/// Returns `{freed (0|1), pool count, global count}`.
pub const SLOT_RELEASE: &str = concat!(
    slot_helpers!(),
    r#"
local totals, held = KEYS[1], KEYS[2]
local pool = ARGV[1]
local pf = 'p|' .. pool
local freed = 0
for _, f in ipairs({ 'g', pf, 't|' .. pool .. '|' .. ARGV[2], 'k|' .. pool .. '|' .. ARGV[3] }) do
  if (tonumber(redis.call('HGET', held, f)) or 0) > 0 then
    adjust(held, f, -1)
    adjust(totals, f, -1)
    if f == pf then freed = 1 end
  end
end
if freed == 1 then wake(totals, pool, ARGV[4], now_ms()) end
local c = redis.call('HMGET', totals, pf, 'g')
return { freed, tonumber(c[1]) or 0, tonumber(c[2]) or 0 }
"#
);

/// Replace everything this replica is recorded as holding.
///
/// KEYS[1] = slot totals hash, KEYS[2] = this replica's holdings,
/// KEYS[3] = replica set
/// ARGV[1] = instance id, ARGV[2] = release channel,
/// ARGV[3..] = field, count pairs (the fields of the totals hash)
///
/// The old holdings are taken out of the totals and the new ones put in, so
/// drift from a lost release or a timed-out call is repaired. Refused (and
/// nothing written) when the replica is not live, like a claim. Publishes
/// for any pool whose count this lowered while it is waited on.
///
/// Returns 1, or -4 when the replica is not live.
pub const SLOT_RECONCILE: &str = concat!(
    slot_helpers!(),
    r#"
local totals, held = KEYS[1], KEYS[2]
local now = now_ms()
local live = tonumber(redis.call('ZSCORE', KEYS[3], ARGV[1]))
if live == nil or live <= now then
  return -4
end

local new = {}
for i = 3, #ARGV, 2 do
  local n = tonumber(ARGV[i + 1]) or 0
  if n > 0 then new[ARGV[i]] = n end
end
local old = {}
local h = redis.call('HGETALL', held)
for i = 1, #h, 2 do old[h[i]] = tonumber(h[i + 1]) or 0 end

local lowered = {}
for f, n in pairs(old) do
  local d = (new[f] or 0) - n
  if d ~= 0 then adjust(totals, f, d) end
  if d < 0 and string.sub(f, 1, 2) == 'p|' then table.insert(lowered, string.sub(f, 3)) end
end
for f, n in pairs(new) do
  if old[f] == nil then adjust(totals, f, n) end
end

redis.call('DEL', held)
for f, n in pairs(new) do redis.call('HSET', held, f, n) end
for _, pool in ipairs(lowered) do wake(totals, pool, ARGV[2], now) end
return 1
"#
);
