import { describe, it, expect } from "vitest";
import type { UsageDailyRow } from "@/lib/obleth";
import {
  ALL_COLUMNS,
  buildUsageCsv,
  csvField,
  selectColumns,
  type ExportContext,
} from "./usage-export";

const TENANT = "11111111-1111-1111-1111-111111111111";
const KEY = "22222222-2222-2222-2222-222222222222";
const EMPTY = "00000000-0000-0000-0000-000000000000";

function makeRow(overrides: Partial<UsageDailyRow> = {}): UsageDailyRow {
  return {
    day: "2026-07-01",
    tenant_id: TENANT,
    key_id: KEY,
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
    cost_usd: 1.2345,
    energy_wh: 2500,
    energy_cost_usd: 0.25,
    co2_g: 900,
    ...overrides,
  };
}

function makeCtx(overrides: Partial<ExportContext> = {}): ExportContext {
  return {
    startDay: "2026-07-01",
    endDay: "2026-07-02",
    tenantNames: new Map([[TENANT, "physics-101"]]),
    keyNames: new Map([[KEY, "alice"]]),
    keyPrefixes: new Map([[KEY, "obl_a1b2"]]),
    ...overrides,
  };
}

describe("selectColumns", () => {
  it("returns all columns when nothing requested", () => {
    expect(selectColumns(null)).toEqual([...ALL_COLUMNS]);
  });

  it("filters to the requested subset in ALL_COLUMNS order, dropping unknowns", () => {
    expect(selectColumns("cost_usd,bogus,key_name")).toEqual(["key_name", "cost_usd"]);
  });

  it("falls back to all columns when the request matches nothing", () => {
    expect(selectColumns("bogus")).toEqual([...ALL_COLUMNS]);
  });
});

describe("csvField", () => {
  it("quotes fields containing commas and escapes quotes", () => {
    expect(csvField('a,"b"')).toBe('"a,""b"""');
    expect(csvField("plain")).toBe("plain");
  });
});

describe("buildUsageCsv", () => {
  it("emits cost_usd as-is (frozen cost, never recomputed)", () => {
    const csv = buildUsageCsv([makeRow()], ["cost_usd"], makeCtx());
    expect(csv).toBe("cost_usd\r\n1.2345");
  });

  it("resolves key_name and tenant_name from the lookup maps", () => {
    const csv = buildUsageCsv([makeRow()], ["tenant_name", "key_name", "key_prefix"], makeCtx());
    expect(csv.split("\r\n")[1]).toBe("physics-101,alice,obl_a1b2");
  });

  it("degrades to blank when a name lookup misses", () => {
    const ctx = makeCtx({ keyNames: new Map() });
    const csv = buildUsageCsv([makeRow()], ["key_name", "key_id"], ctx);
    expect(csv.split("\r\n")[1]).toBe(`,${KEY}`);
  });

  it("blanks id and name cells for the empty (rolled-up) uuid", () => {
    const row = makeRow({ tenant_id: EMPTY, key_id: EMPTY });
    const csv = buildUsageCsv([row], ["tenant_id", "tenant_name", "key_id", "key_name"], makeCtx());
    expect(csv.split("\r\n")[1]).toBe(",,,");
  });

  it("converts energy_wh to energy_kwh", () => {
    const csv = buildUsageCsv([makeRow()], ["energy_kwh"], makeCtx());
    expect(csv.split("\r\n")[1]).toBe("2.5");
  });
});
