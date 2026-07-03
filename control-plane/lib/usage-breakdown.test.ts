import { describe, it, expect } from "vitest";
import type { UsageDailyRow } from "@/lib/obleth";
import { formatUsd, toBreakdownRows, type NameLookups } from "./usage-breakdown";

const T1 = "11111111-1111-1111-1111-111111111111";
const K1 = "22222222-2222-2222-2222-222222222222";

function makeRow(overrides: Partial<UsageDailyRow> = {}): UsageDailyRow {
  return {
    day: "2026-07-01",
    tenant_id: T1,
    key_id: K1,
    model: "gemma4-31b-it",
    requests: 10,
    success_requests: 9,
    error_requests: 1,
    input_tokens: 1000,
    output_tokens: 500,
    total_tokens: 1500,
    estimated_tokens: 1400,
    cache_hits: 2,
    cache_misses: 8,
    avg_ttft_ms: 120.5,
    avg_total_ms: 900.4,
    cost_usd: 1.5,
    energy_wh: 2500,
    energy_cost_usd: 0.25,
    co2_g: 900,
    ...overrides,
  };
}

function lookups(overrides: Partial<NameLookups> = {}): NameLookups {
  return {
    tenantNames: new Map([[T1, "physics-101"]]),
    keyNames: new Map([[K1, "alice"]]),
    keyPrefixes: new Map([[K1, "obl_a1b2"]]),
    ...overrides,
  };
}

describe("toBreakdownRows", () => {
  it("day grouping stays chronological with the day as label", () => {
    const rows = [makeRow({ day: "2026-07-02" }), makeRow({ day: "2026-07-01" })];
    const out = toBreakdownRows(rows, "day", lookups());
    expect(out.map((r) => r.label)).toEqual(["2026-07-01", "2026-07-02"]);
  });

  it("tenant grouping labels by name and sorts by spend descending", () => {
    const t2 = "33333333-3333-3333-3333-333333333333";
    const rows = [
      makeRow({ cost_usd: 1 }),
      makeRow({ tenant_id: t2, cost_usd: 5 }),
    ];
    const lk = lookups({
      tenantNames: new Map([
        [T1, "physics-101"],
        [t2, "chem-202"],
      ]),
    });
    const out = toBreakdownRows(rows, "tenant", lk);
    expect(out.map((r) => r.label)).toEqual(["chem-202", "physics-101"]);
  });

  it("falls back to a truncated id when a tenant name is unknown", () => {
    const out = toBreakdownRows([makeRow()], "tenant", lookups({ tenantNames: new Map() }));
    expect(out[0].label).toBe(T1.slice(0, 8));
  });

  it("key grouping shows the key name with its prefix as sublabel", () => {
    const out = toBreakdownRows([makeRow()], "key", lookups());
    expect(out[0].label).toBe("alice");
    expect(out[0].sublabel).toBe("obl_a1b2");
  });

  it("falls back to the truncated id when a key name is empty", () => {
    const lk = lookups({ keyNames: new Map([[K1, ""]]) });
    const out = toBreakdownRows([makeRow()], "key", lk);
    expect(out[0].label).toBe(K1.slice(0, 8));
  });

  it("model grouping labels unknown models", () => {
    const out = toBreakdownRows([makeRow({ model: "" })], "model", lookups());
    expect(out[0].label).toBe("(unknown)");
  });
});

describe("formatUsd", () => {
  it("em-dash for zero", () => {
    expect(formatUsd(0)).toBe("—");
  });
  it("floors tiny values", () => {
    expect(formatUsd(0.005)).toBe("< $0.01");
  });
  it("two decimals otherwise", () => {
    expect(formatUsd(1.234)).toBe("$1.23");
  });
});
