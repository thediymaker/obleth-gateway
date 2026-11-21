import { describe, expect, it } from "vitest";
import {
  buildTenantRows,
  countKeys,
  accessState,
  budgetView,
  buildKeyRows,
  changeVs,
  fillDays,
  keyFiltersWords,
  keyListParams,
  keyListQuery,
  parseKeyListParams,
  EMPTY_KEY_FILTERS,
  gridToWindows,
  groupShare,
  lastUsed,
  reachLabel,
  reachOf,
  tenantHref,
  utcToZoned,
  windowsSummary,
  windowsToGrid,
  zonedToUtc,
} from "./access-model";
import type { ApiKey, BudgetUsage, KeyUsageSummary, Tenant } from "./obleth";

const key = (over: Partial<ApiKey> = {}): ApiKey => ({
  id: "k1", tenant_id: "t1", name: "alice", description: "", key_prefix: "sk-abc", kind: "secret",
  identity_issuer: null, identity_subject: null, identity_claims: null, weight: 100, max_in_flight: null,
  budget_tokens: null, budget_cost_usd: null, budget_period: null, budget_started_at: null,
  disabled: false, tracing_enabled: false, end_user_fairshare: false, background: false, created_at: "2026-09-01T00:00:00Z", updated_at: "", ...over,
});

describe("time zones", () => {
  it("turns a Phoenix wall clock into UTC and back", () => {
    expect(zonedToUtc("2026-08-17T08:00", "America/Phoenix")).toBe("2026-08-17T15:00:00.000Z");
    expect(utcToZoned("2026-08-17T15:00:00Z", "America/Phoenix")).toBe("2026-08-17T08:00");
  });

  it("lands on the right side of a daylight-saving change", () => {
    // New York is UTC-4 in July, UTC-5 in December.
    expect(zonedToUtc("2026-07-01T09:00", "America/New_York")).toBe("2026-07-01T13:00:00.000Z");
    expect(zonedToUtc("2026-12-01T09:00", "America/New_York")).toBe("2026-12-01T14:00:00.000Z");
  });
});

describe("access hours", () => {
  const weekdays = [1, 2, 3, 4, 5].map((day) => ({ day, start_min: 420, end_min: 1320 }));

  it("groups days with the same hours", () => {
    expect(windowsSummary(weekdays)).toBe("Weekdays 7:00–22:00");
    expect(windowsSummary([...weekdays, { day: 6, start_min: 540, end_min: 1020 }])).toBe("Weekdays 7:00–22:00 · Sat 9:00–17:00");
    expect(windowsSummary(null)).toBe("Open all hours");
  });

  it("reads the hours in the tenant's own zone", () => {
    const t = { timezone: "America/Phoenix", active_from: null, active_until: null, weekly_windows: weekdays };
    // Tuesday 22:14 in Phoenix is Wednesday 05:14 UTC.
    expect(accessState(t, new Date("2026-09-30T05:14:00Z"))).toBe("closed");
    expect(accessState(t, new Date("2026-09-29T17:00:00Z"))).toBe("open");
    expect(accessState({ ...t, active_until: "2026-09-01T00:00:00Z" }, new Date("2026-09-29T17:00:00Z"))).toBe("ended");
  });

  it("round-trips the week grid", () => {
    const windows = [{ day: 1, start_min: 420, end_min: 1320 }, { day: 6, start_min: 540, end_min: 1020 }];
    expect(gridToWindows(windowsToGrid(windows))).toEqual(windows);
  });
});

describe("budgets", () => {
  const month: BudgetUsage = {
    scope: "tenant", id: "t1", tenant_id: "t1", period: "monthly", budget_tokens: null, budget_cost_usd: 1000,
    used_tokens: 0, used_cost_usd: 612, period_start: "2026-09-01T07:00:00Z", resets_at: "2026-10-01T07:00:00Z",
  };

  it("says how much of the period is used, and where the pace lands", () => {
    const v = budgetView(month, new Date("2026-09-20T07:00:00Z"))!;
    expect(v.label).toBe("$612 of $1,000 this month");
    expect(v.share).toBeCloseTo(0.612);
    expect(v.pace).toBe("on pace for $966");
  });

  it("gives no pace early in the month, or for a term", () => {
    expect(budgetView(month, new Date("2026-09-02T07:00:00Z"))!.pace).toBeNull();
    expect(budgetView({ ...month, period: "term", resets_at: null })!.pace).toBeNull();
  });

  it("goes by whichever cap is nearer", () => {
    const v = budgetView({ ...month, budget_tokens: 1_000_000, used_tokens: 900_000 })!;
    expect(v.label).toBe("900K of 1M tokens this month");
  });
});

describe("keys", () => {
  const usage = (key_id: string, requests: number, last_used_ms: number): KeyUsageSummary => ({
    key_id, tenant_id: "t1", last_used_ms, last_model: "", last_status_code: 200, requests,
    input_tokens: 0, output_tokens: 0, total_tokens: 0, cost_usd: 0, energy_wh: 0, energy_cost_usd: 0, co2_g: 0,
  });
  const now = Date.parse("2026-09-28T00:00:00Z");
  const rows = buildKeyRows(
    [key(), key({ id: "k2", name: "bob", disabled: true }), key({ id: "k3", name: "mlee42", kind: "identity", identity_subject: "mlee42@asu.edu" })],
    [{ id: "t1", name: "rcusers" }],
    [usage("k1", 10, now - 60_000), usage("k3", 50, now - 3_600_000)],
    [],
  );

  it("joins a page's use to its keys", () => {
    expect(rows.map((r) => [r.key.id, r.usage?.requests ?? 0, r.lastUsedMs, r.tenantName])).toEqual([
      ["k1", 10, now - 60_000, "rcusers"],
      ["k2", 0, 0, "rcusers"],
      ["k3", 50, now - 3_600_000, "rcusers"],
    ]);
  });

  it("asks the gateway to filter and sort every key", () => {
    const window = { sinceMs: 1, untilMs: 2 };
    expect(keyListQuery({ ...EMPTY_KEY_FILTERS, query: " lab ", tenantId: "t1", status: "off", kind: "identity", budget: "near", unused: true }, "spend", window)).toEqual({
      tenantId: "t1", q: "lab", disabled: true, kind: "identity", budget: "near", unused: true, sort: "cost", sinceMs: 1, untilMs: 2,
    });
    expect(keyListQuery(EMPTY_KEY_FILTERS, "newest", window)).toEqual({
      tenantId: undefined, q: undefined, disabled: undefined, kind: undefined, budget: undefined, unused: undefined, sort: "created", dir: "desc", sinceMs: 1, untilMs: 2,
    });
    expect(keyListQuery({ ...EMPTY_KEY_FILTERS, status: "on" }, "last-used", window)).toMatchObject({ disabled: false, sort: "last_used" });
  });

  it("carries the query through the live route unchanged", () => {
    const q = keyListQuery({ ...EMPTY_KEY_FILTERS, query: "a&b", status: "on", budget: "capped" }, "name", { sinceMs: 10, untilMs: 20 });
    const params = keyListParams(q);
    expect(params.toString()).toBe("q=a%26b&disabled=false&budget=capped&sort=name&since_ms=10&until_ms=20");
    expect(parseKeyListParams(params)).toEqual({ ...q, tenantId: undefined, ids: undefined, kind: undefined, unused: undefined, dir: undefined });
    expect(parseKeyListParams(new URLSearchParams("sort=weight&kind=x&since_ms=abc"))).toMatchObject({ sort: undefined, kind: undefined, sinceMs: undefined });
  });

  it("says what a select-all-by-filter covers", () => {
    expect(keyFiltersWords(EMPTY_KEY_FILTERS)).toBe("every key");
    expect(keyFiltersWords({ ...EMPTY_KEY_FILTERS, query: "lab", tenantId: "t1", status: "off" }, "Research")).toBe("matching “lab”, in Research, turned off");
  });

  it("words when a key was last used", () => {
    expect(lastUsed(now - 12_000, now)).toBe("12 s ago");
    expect(lastUsed(now - 41 * 86_400_000, now)).toBe("41 days ago");
    expect(lastUsed(0, now)).toBe("never");
  });
});

describe("tenants", () => {
  it("previews a weight's share of its group", () => {
    const tenants = [
      { id: "a", fairshare_group: "default", weight: 100, status: "active" },
      { id: "b", fairshare_group: "default", weight: 100, status: "active" },
      { id: "c", fairshare_group: "default", weight: 150, status: "active" },
      { id: "d", fairshare_group: "default", weight: 500, status: "suspended" },
    ];
    expect(groupShare(tenants, "c", "default", 200)).toBeCloseTo(0.5);
    expect(groupShare(tenants, "c", "new-group", 200)).toBe(1);
  });

  it("links by name", () => {
    expect(tenantHref({ name: "brain training" }, "budget")).toBe("/tenants/brain%20training#budget");
  });

  it("fills every day of the chart and words the change", () => {
    expect(fillDays([{ day: "2026-09-02", requests: 3, failed: 1, cost: 0, tokens: 0 }], "2026-09-01", "2026-09-03").map((d) => d.requests)).toEqual([0, 3, 0]);
    expect(changeVs(132, 100)).toBe("▲ 32% vs yesterday");
    expect(changeVs(5, 0)).toBe("new since yesterday");
    expect(changeVs(5, null)).toBeNull();
  });
});

describe("what a person can reach", () => {
  it("needs a tenant for the portal, and approval for anything", () => {
    expect(reachLabel(reachOf({ role: "admin", status: "active", tenantId: null }))).toBe("Dashboard");
    expect(reachLabel(reachOf({ role: "admin", status: "active", tenantId: "t" }), "rcusers")).toBe("Dashboard and portal · rcusers");
    expect(reachLabel(reachOf({ role: "user", status: "active", tenantId: null }))).toBe("Nothing yet: no tenant");
    expect(reachOf({ role: "user", status: "pending", tenantId: "t" })).toBe("waiting");
  });
});

describe("tenant rows from counts", () => {
  it("count each tenant's keys and people without a scan per tenant", () => {
    const counts = countKeys([
      { tenant_id: "t1", disabled: false },
      { tenant_id: "t1", disabled: true },
      { tenant_id: "t2", disabled: false },
    ]);
    expect(counts).toEqual({ t1: { keys: 2, off: 1 }, t2: { keys: 1, off: 0 } });
    const tenants = [{ id: "t1", name: "a" }, { id: "t3", name: "c" }] as unknown as Tenant[];
    const rows = buildTenantRows(tenants, counts, [{ tenantId: "t1" }, { tenantId: "t1" }, { tenantId: null }], [], []);
    expect(rows.map((r) => [r.keys, r.keysOff, r.users])).toEqual([[2, 1, 2], [0, 0, 0]]);
  });

  it("summarise a key's networks once, as rows are built", () => {
    const [open, limited] = buildKeyRows(
      [key(), key({ id: "k2", allowed_networks: ["10.0.0.0/8", "192.0.2.0/24"] })],
      [{ id: "t1", name: "rcusers" }],
      [],
      [],
    );
    expect(open.networks).toBeNull();
    expect(limited.networks).toBe("Only 2 networks");
  });
});
