// Shared primitives for the obleth benchmark scripts.
//
// Two HTTP paths are intentionally kept separate:
//   * `adminApi()` uses global `fetch` for low-volume management calls.
//   * `LoadClient` uses node:http/https with a tuned keep-alive agent so the
//     load generator can drive thousands of concurrent requests with pooled
//     connections and capture per-request TTFB precisely. The default fetch
//     dispatcher caps connections per origin, which silently throttles
//     throughput tests, so the raw client is used on the hot path.

import http from "node:http";
import https from "node:https";

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))]);
}

export function mean(values) {
  if (!values.length) return 0;
  return Math.round(values.reduce((sum, v) => sum + v, 0) / values.length);
}

export function truncate(value, max) {
  const s = String(value);
  return s.length <= max ? s : `${s.slice(0, max - 1)}...`;
}

// Minimal admission/usage stats recorder shared by the load scenarios.
export function newStats() {
  return {
    ok: 0,
    rejected: 0,
    error: 0,
    bytes: 0,
    ttfb: [],
    total: [],
    statuses: {},
    sampleErrors: [],
  };
}

export function record(stats, result) {
  const key = String(result.status);
  stats.statuses[key] = (stats.statuses[key] ?? 0) + 1;
  stats.bytes += result.bytes ?? 0;
  if (result.status === 200) {
    stats.ok++;
    stats.ttfb.push(result.ttfbMs);
    stats.total.push(result.totalMs);
  } else if (result.status === 429) {
    stats.rejected++;
  } else {
    stats.error++;
    if (stats.sampleErrors.length < 3) {
      stats.sampleErrors.push({
        status: result.status || "network",
        body: truncate((result.error ?? result.body ?? "").replace(/\s+/g, " "), 240),
      });
    }
  }
}

export function summarizeStats(stats) {
  const attempts = stats.ok + stats.rejected + stats.error;
  return {
    attempts,
    completed: stats.ok,
    rejected: stats.rejected,
    errors: stats.error,
    error_rate: attempts ? stats.error / attempts : 0,
    p50_ttfb_ms: percentile(stats.ttfb, 50),
    p90_ttfb_ms: percentile(stats.ttfb, 90),
    p99_ttfb_ms: percentile(stats.ttfb, 99),
    p50_total_ms: percentile(stats.total, 50),
    p90_total_ms: percentile(stats.total, 90),
    p99_total_ms: percentile(stats.total, 99),
  };
}

export function formatCounts(counts) {
  return Object.entries(counts)
    .sort(([a], [b]) => Number(a) - Number(b))
    .map(([status, count]) => `${status}:${count}`)
    .join(" ");
}

// Low-overhead HTTP client over a shared keep-alive agent. Returns timing and
// drains the body so connections are reused. Captures TTFB (first byte), which
// is the meaningful streaming latency, and total time.
export class LoadClient {
  constructor({ maxSockets = 1024 } = {}) {
    const opts = { keepAlive: true, maxSockets, maxFreeSockets: maxSockets, scheduling: "fifo" };
    this.agents = {
      "http:": new http.Agent(opts),
      "https:": new https.Agent(opts),
    };
  }

  request({ url, method = "GET", headers = {}, body }) {
    const u = new URL(url);
    const lib = u.protocol === "https:" ? https : http;
    const agent = this.agents[u.protocol];
    const payload = body === undefined ? undefined : Buffer.from(body);
    const finalHeaders = { ...headers };
    if (payload !== undefined) finalHeaders["content-length"] = payload.length;

    return new Promise((resolve) => {
      const startedAt = Date.now();
      const start = performance.now();
      let ttfb = 0;
      let bytes = 0;
      let tail = "";

      const req = lib.request(
        {
          protocol: u.protocol,
          hostname: u.hostname,
          port: u.port,
          path: u.pathname + u.search,
          method,
          headers: finalHeaders,
          agent,
        },
        (res) => {
          res.on("data", (chunk) => {
            if (!ttfb) ttfb = performance.now() - start;
            bytes += chunk.length;
            // Keep only a small tail so error bodies are visible without
            // retaining full streamed payloads under high concurrency.
            if (res.statusCode !== 200 && tail.length < 512) {
              tail += chunk.toString("utf8");
            }
          });
          res.on("end", () => {
            const totalMs = performance.now() - start;
            resolve({
              status: res.statusCode,
              ttfbMs: ttfb || totalMs,
              totalMs,
              bytes,
              startedAt,
              body: tail,
            });
          });
          res.on("error", (err) =>
            resolve({ status: 0, error: err.message, ttfbMs: 0, totalMs: performance.now() - start, bytes, startedAt }),
          );
        },
      );
      req.on("error", (err) =>
        resolve({ status: 0, error: err.message, ttfbMs: 0, totalMs: performance.now() - start, bytes, startedAt }),
      );
      if (payload !== undefined) req.write(payload);
      req.end();
    });
  }

  destroy() {
    for (const agent of Object.values(this.agents)) agent.destroy();
  }
}

// Management API client. Low volume, so plain fetch is fine.
export function adminApi(base, token) {
  return async function api(path, method = "GET", body) {
    const res = await fetch(`${base}/api/v1${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) throw new Error(`${method} ${path} -> ${res.status}: ${await res.text()}`);
    return res.status === 204 ? null : res.json();
  };
}

// Weighted random choice over [{ value, weight }] entries.
export function weightedPick(entries) {
  const total = entries.reduce((sum, e) => sum + e.weight, 0);
  let r = Math.random() * total;
  for (const entry of entries) {
    r -= entry.weight;
    if (r <= 0) return entry.value;
  }
  return entries[entries.length - 1].value;
}

// ---- Seeding helpers (shared by the load scenarios) ----------------------

export async function ensureModel(api, spec) {
  const models = await api("/models");
  const existing = models.find((m) => m.model_name === spec.model_name);
  if (existing) {
    return api(`/models/${existing.id}`, "PUT", {
      upstream_model: spec.upstream_model,
      api_base: spec.api_base,
      api_key: existing.api_key ?? null,
      input_cost_per_token: spec.input_cost_per_token ?? existing.input_cost_per_token ?? 0,
      output_cost_per_token: spec.output_cost_per_token ?? existing.output_cost_per_token ?? 0,
      context_window: spec.context_window ?? existing.context_window ?? 8192,
      admission_weight: spec.admission_weight ?? existing.admission_weight ?? 100,
      supports_function_calling: existing.supports_function_calling ?? false,
      supports_system_messages: existing.supports_system_messages ?? true,
      supports_response_schema: existing.supports_response_schema ?? false,
      supports_tool_choice: existing.supports_tool_choice ?? false,
      enabled: true,
    });
  }
  return api("/models", "POST", {
    model_name: spec.model_name,
    upstream_model: spec.upstream_model,
    api_base: spec.api_base,
    context_window: spec.context_window ?? 8192,
    admission_weight: spec.admission_weight ?? 100,
  });
}

export async function ensureGroup(api, name, weight) {
  const existing = (await api("/fairshare/groups")).find((g) => g.name === name);
  if (existing) {
    await api(`/fairshare/groups/${encodeURIComponent(name)}/weight`, "PATCH", { weight });
    return { ...existing, weight };
  }
  return api("/fairshare/groups", "POST", { name, weight });
}

export async function ensureTenant(api, { name, weight, tokensPerMinute, fairshareGroup }) {
  const existing = (await api("/tenants")).find((t) => t.name === name);
  if (existing) {
    await api(`/tenants/${existing.id}/weight`, "PATCH", { weight });
    await api(`/tenants/${existing.id}/quota`, "PUT", {
      tokens_per_minute: tokensPerMinute,
      max_in_flight: null,
    });
    if (fairshareGroup && existing.fairshare_group !== fairshareGroup) {
      await api(`/tenants/${existing.id}/group`, "PATCH", { fairshare_group: fairshareGroup });
    }
    return { ...existing, weight, tokens_per_minute: tokensPerMinute, fairshare_group: fairshareGroup };
  }
  return api("/tenants", "POST", {
    name,
    weight,
    tokens_per_minute: tokensPerMinute,
    fairshare_group: fairshareGroup,
  });
}

// Replace any same-named keys for the tenant, then mint a fresh secret. Keeps
// repeated benchmark runs from accumulating dead keys.
export async function mintKey(api, tenant, keyName) {
  const inventory = await api("/keys");
  const stale = inventory.filter((k) => k.tenant_id === tenant.id && k.name === keyName);
  for (const k of stale) {
    try {
      await api(`/keys/${k.id}`, "DELETE");
    } catch {
      // Best effort; a leftover key does not invalidate the run.
    }
  }
  const minted = await api(`/tenants/${tenant.id}/keys`, "POST", { name: keyName });
  return minted.secret;
}

export async function setCapacity(api, maxInFlight) {
  const body = await api("/capacity", "PUT", { max_in_flight: maxInFlight });
  return body.max_in_flight;
}
