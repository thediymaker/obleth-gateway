import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ExportSheet } from "./export-sheet";
import type { UsageDailyRow } from "@/lib/obleth";

const row = (over: Partial<UsageDailyRow> = {}): UsageDailyRow => ({
  day: "", tenant_id: "t1", key_id: "00000000-0000-0000-0000-000000000000", model: "", requests: 512004, success_requests: 509900, error_requests: 2104,
  input_tokens: 640, output_tokens: 210, total_tokens: 850, estimated_tokens: 0, cache_hits: 0, cache_misses: 0,
  avg_ttft_ms: 410, avg_total_ms: 2400, cost_usd: 1904.12, energy_wh: 520100, energy_cost_usd: 62.41, co2_g: 166400, ...over,
});

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;
let assigned = "";

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  localStorage.clear();
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const body = url.includes("group_by=day_key_model")
      ? [row({ day: "2026-09-27", key_id: "k1", model: "glm-5-3" }), row({ day: "2026-09-26", key_id: "k1", model: "glm-5-3" })]
      : [row(), row({ tenant_id: "t2", cost_usd: 12 })];
    return { ok: true, json: async () => body } as Response;
  }));
  Object.defineProperty(window, "location", { configurable: true, value: { ...window.location, set href(v: string) { assigned = v; }, get href() { return assigned; } } });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <ExportSheet
          open
          onClose={() => {}}
          range={{ start: "2026-09-01", end: "2026-09-27" }}
          filters={{ tenantId: "", keyId: "", model: "" }}
          filterLine="every team"
          names={{ tenantNames: new Map([["t1", "research-computing"], ["t2", "voyager"]]), keyNames: new Map([["k1", "rc-batch"]]), keyPrefixes: new Map([["k1", "sk-rc4"]]) }}
        />
      </QueryClientProvider>,
    );
  });
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
}

const button = (text: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.startsWith(text)) as HTMLButtonElement;

describe("exporting usage", () => {
  it("starts from one row per team and previews the cells the file will hold", async () => {
    await render();
    const table = document.querySelector("table")!;
    expect([...table.querySelectorAll("th")].map((t) => t.textContent)).toEqual(["Start date", "End date", "Team", "Requests", "Failed", "Tokens in", "Tokens out", "Total tokens", "Spend (USD)", "Energy (kWh)"]);
    expect(table.querySelector("tbody tr")!.textContent).toContain("research-computing");
    expect(table.querySelector("tbody tr")!.textContent).toContain("520.1");
    expect(button("Download CSV").textContent).toContain("2 rows");
  });

  it("switches to the most detailed rows with a preset", async () => {
    await render();
    await act(async () => button("Every day, key and model").click());
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    expect(calls.some((c) => c.includes("group_by=day_key_model"))).toBe(true);
    const first = document.querySelector("tbody tr")!.textContent!;
    expect(first).toContain("2026-09-27");
    expect(first).toContain("rc-batch");
    expect(first).toContain("glm-5-3");
  });

  it("downloads with the chosen grouping and columns, remembering them when asked", async () => {
    await render();
    await act(async () => button("✓ Energy (kWh)").click());
    await act(async () => button("+ Cache hits").click());
    const remember = document.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    await act(async () => remember.click());
    await act(async () => button("Download CSV").click());
    const url = new URL(assigned, "http://x");
    expect(url.pathname).toBe("/api/live/usage/export");
    expect(url.searchParams.get("group_by")).toBe("tenant");
    expect(url.searchParams.get("columns")).toBe("start_day,end_day,tenant_name,requests,error_requests,input_tokens,output_tokens,total_tokens,cache_hits,cost_usd");
    expect(JSON.parse(localStorage.getItem("obleth:reports:export-columns")!)).toContain("cache_hits");
  });
});
