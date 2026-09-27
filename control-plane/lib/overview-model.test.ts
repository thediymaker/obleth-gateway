import { describe, expect, it } from "vitest";
import type { AuditEntry, FairshareLiveView, ModelHealthSummary, ModelRoute, UsageModelAgg } from "@/lib/obleth";
import { EMPTY_OVERVIEW_SUMMARY } from "@/lib/overview-summary";
import { buildAttention, buildFleet, buildSeries, changeLabel, describeAudit, previousTotals } from "./overview-model";

const HOUR = 3_600_000;
const model = (name: string, extra: Partial<ModelRoute> = {}) =>
  ({ id: `id-${name}`, model_name: name, upstream_model: name, api_base: "http://backend", model_type: "chat", enabled: true, max_in_flight: 8, tags: [], ...extra }) as ModelRoute;
const health = (name: string, status: string, extra: Partial<ModelHealthSummary> = {}) =>
  ({ model_id: `id-${name}`, model_name: name, status, consecutive_failures: 0, maintenance_until: null, last_http_status: null, last_message: null, last_checked_at: null, ...extra }) as ModelHealthSummary;
const fairshare = (extra: Partial<FairshareLiveView> = {}) =>
  ({ algorithm: "drr", max_in_flight: 64, global_in_flight: 0, global_queued: 0, groups: [], tenants: [], ...extra }) as FairshareLiveView;

describe("buildSeries", () => {
  it("lays the current window out bucket by bucket, each with the bucket one window earlier", () => {
    const now = 10 * HOUR + 90_000; // 10:01:30
    const at = (t: number, requests: number, errors = 0) => ({ bucket_ms: t, requests, input_tokens: 0, output_tokens: 0, total_tokens: requests * 10, errors, p50_ttft_ms: 300 });
    const points = [at(10 * HOUR, 5, 1), at(9 * HOUR, 3), at(10 * HOUR - 60_000, 2)];
    const series = buildSeries(points, "1h", now);
    expect(series).toHaveLength(60);
    expect(series.at(-1)).toMatchObject({ t: 10 * HOUR + 60_000, requests: 0, prevRequests: 0 });
    // 10:00 this window lines up with 09:00 the hour before.
    expect(series.at(-2)).toMatchObject({ t: 10 * HOUR, requests: 5, errors: 1, tokens: 50, ttft: 300, prevRequests: 3 });
    // Missing buckets are zero, never skipped.
    expect(series.filter((p) => p.requests === 0 && p.t < 10 * HOUR - 60_000)).toHaveLength(57);
  });

  it("reads latency as missing, not zero, when a bucket produced no first token", () => {
    const series = buildSeries([{ bucket_ms: 0, requests: 1, input_tokens: 0, output_tokens: 0, total_tokens: 0, p50_ttft_ms: 0 }], "1h", 30_000);
    expect(series.at(-1)?.ttft).toBeNull();
  });
});

describe("window comparisons", () => {
  it("derives the previous window by subtracting the current one from a two-window read", () => {
    const current = { ...EMPTY_OVERVIEW_SUMMARY, requests: 120, tokens: 1000, errors: 3, cost: 2 };
    const both = { ...EMPTY_OVERVIEW_SUMMARY, requests: 220, tokens: 1800, errors: 4, cost: 3.5 };
    expect(previousTotals(current, both)).toEqual({ requests: 100, tokens: 800, errors: 1, cost: 1.5 });
  });

  it("says which way things moved, and stays quiet with nothing to compare", () => {
    expect(changeLabel(111, 100, "yesterday")).toBe("▲ 11% vs yesterday");
    expect(changeLabel(96, 100, "yesterday")).toBe("▼ 4.0% vs yesterday");
    expect(changeLabel(100, 100, "yesterday")).toBe("Same as yesterday");
    expect(changeLabel(5, 0, "yesterday")).toBeNull();
  });
});

describe("buildFleet", () => {
  it("puts failing models first, then full pools, then the busiest", () => {
    const models = [model("idle"), model("busy"), model("down"), model("full"), model("new"), model("off", { enabled: false }), model("mock-model-a")];
    const fleet = buildFleet(
      models,
      [health("idle", "healthy"), health("busy", "healthy"), health("down", "unhealthy"), health("full", "healthy")],
      fairshare({ model_in_flight: { busy: 6, full: 8 }, model_queued: { full: 3 } }),
      [{ model: "idle", requests: 50, p50_ttft_ms: 200 } as UsageModelAgg],
    );
    expect(fleet.map((t) => t.name)).toEqual(["down", "full", "busy", "idle", "new"]);
    expect(fleet[0].attention).toBe("down");
    expect(fleet[1]).toMatchObject({ attention: "full", inFlight: 8, cap: 8, queued: 3 });
    expect(fleet.find((t) => t.name === "new")?.health).toBe("unknown");
  });

  it("reads cluster-wide occupancy and the configured pool size in shared mode", () => {
    const [tile] = buildFleet([model("kimi")], [health("kimi", "healthy")], fairshare({
      mode: "shared",
      pools: [{ model: "kimi", cap: 22, configured_cap: 64, in_flight: 20, cluster_in_flight: 58, queued: 0, borrowed: 0, groups: [], tenants: [], keys: [] }],
    }), []);
    expect(tile).toMatchObject({ inFlight: 58, cap: 64, attention: null });
  });

  it("treats a maintenance window as maintenance, whatever the last check said", () => {
    const [tile] = buildFleet([model("m")], [health("m", "unhealthy", { maintenance_until: new Date(Date.now() + HOUR).toISOString() })], undefined, []);
    expect(tile).toMatchObject({ health: "maintenance", attention: null });
  });
});

describe("buildAttention", () => {
  it("orders failing models, full pools and tenants below share, with the evidence", () => {
    const fleet = buildFleet(
      [model("flux"), model("kimi", { max_in_flight: 4 }), model("new"), model("maint")],
      [
        health("flux", "unhealthy", { consecutive_failures: 5, last_http_status: 503, last_message: "upstream connect error" }),
        health("kimi", "healthy"),
        health("maint", "healthy", { maintenance_until: new Date(Date.now() + HOUR).toISOString() }),
      ],
      fairshare({ model_in_flight: { kimi: 4 }, model_queued: { kimi: 2 }, global_queued: 2 }),
      [],
    );
    const { items, watching } = buildAttention(fleet, fairshare({
      global_queued: 2,
      tenants: [{ tenant_id: "t1", name: "cs-teaching", fairshare_group: "", weight: 1, in_flight: 1, queued: 2, served_tokens: 0, share_score: 0, weight_share: 0, expected_slots: 3.6 }],
    }));
    expect(items.map((i) => i.key)).toEqual(["down:flux", "full:kimi", "share:t1"]);
    expect(items[0]).toMatchObject({ badge: "Down", urgent: true });
    expect(items[0].detail).toContain("5 failed checks in a row");
    expect(items[0].detail).toContain("HTTP 503");
    expect(items[1].detail).toBe("4 of 4 slots in use · 2 queued");
    expect(items[2].detail).toContain("against an expected 3");
    expect(watching[0]).toBe("new has no health data yet");
    expect(watching[1]).toMatch(/^maint is in maintenance until /);
  });

  it("reports a queue with no full pool on its own", () => {
    const { items } = buildAttention([], fairshare({ global_queued: 3, tenants: [] }));
    expect(items).toEqual([expect.objectContaining({ key: "queued", title: "3 requests waiting for admission" })]);
  });

  it("is empty on a quiet day", () => {
    expect(buildAttention([], fairshare()).items).toEqual([]);
  });
});

describe("describeAudit", () => {
  const entry = (e: Partial<AuditEntry>): AuditEntry => ({ id: 1, ts: "2026-09-27T00:00:00Z", actor: "jlee379@asu.edu", action: "update_model", entity_type: "model", entity_id: "id-kimi", detail: {}, ...e });
  it("names the target from the registry, then the detail, then a short id", () => {
    const names = { models: new Map([["id-kimi", "kimi-k2"]]), tenants: new Map([["t1", "cs-teaching"]]) };
    expect(describeAudit(entry({}), names)).toEqual({ actor: "jlee379@asu.edu", action: "update model", target: "kimi-k2" });
    expect(describeAudit(entry({ action: "set_tenant_budget", entity_type: "tenant", entity_id: "t1" }), names).target).toBe("cs-teaching");
    expect(describeAudit(entry({ entity_type: "key", entity_id: "k", detail: { name: "ci key" } }), names).target).toBe("ci key");
    expect(describeAudit(entry({ entity_type: "settings", entity_id: "0123456789abcdef", actor: "" }), names)).toMatchObject({ actor: "system", target: "settings 01234567" });
  });
});
