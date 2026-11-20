import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReportsDashboard } from "./reports-dashboard";

vi.mock("next/navigation", () => ({ useRouter: () => ({ push: vi.fn() }) }));
// The real chart builder, counted, to see what a keystroke recomputes.
const chartBuilds = vi.hoisted(() => ({ count: 0 }));
vi.mock("@/lib/reports-model", async (load) => {
  const real = await load<typeof import("@/lib/reports-model")>();
  return { ...real, buildChart: (...args: Parameters<typeof real.buildChart>) => { chartBuilds.count++; return real.buildChart(...args); } };
});
vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));
import type { UsageDailyRow } from "@/lib/obleth";

const EMPTY = "00000000-0000-0000-0000-000000000000";
const row = (over: Partial<UsageDailyRow> = {}): UsageDailyRow => ({
  day: "", tenant_id: EMPTY, key_id: EMPTY, model: "", requests: 100, success_requests: 99, error_requests: 1,
  input_tokens: 700, output_tokens: 300, total_tokens: 1000, estimated_tokens: 0, cache_hits: 10, cache_misses: 90,
  avg_ttft_ms: 400, avg_total_ms: 2000, cost_usd: 10, energy_wh: 0, energy_cost_usd: 0, co2_g: 0, ...over,
});

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  // recharts sizes its chart with a ResizeObserver, which jsdom lacks.
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date(Date.UTC(2026, 8, 27, 12)));
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const p = new URL(url, "http://x").searchParams;
    const group = p.get("group_by");
    const previous = p.get("start_day") === "2026-08-01";
    const body =
      group === "day" ? [row({ day: previous ? "2026-08-02" : "2026-09-02", requests: previous ? 100 : 118 })]
      : group === "tenant" ? [row({ tenant_id: "t1", cost_usd: 30 }), row({ tenant_id: "t2", cost_usd: 10 })]
      : group === "model" ? [row({ model: "glm-5-3", total_tokens: 5000 })]
      : [row({ day: "2026-09-02", tenant_id: "t1" })];
    return { ok: true, json: async () => body } as Response;
  }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function render() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <ReportsDashboard
          tenants={[{ id: "t1", name: "research-computing" }, { id: "t2", name: "voyager" }]}
          keys={[]}
          models={["glm-5-3"]}
        />
      </QueryClientProvider>,
    );
  });
  await act(async () => { await vi.waitFor(() => expect(host.textContent).toContain("118")); });
}

describe("the Reports page", () => {
  it("reads this month so far against the same days last month", async () => {
    await render();
    expect(host.textContent).toContain("Sep 1 – Sep 27, 2026 UTC · every team · compared with Aug 1 – Aug 27, 2026 UTC");
    expect(calls.some((c) => c.includes("start_day=2026-08-01") && c.includes("end_day=2026-08-27"))).toBe(true);
    expect(host.textContent).toContain("▲ 18% vs the same days last month");
  });

  it("links the failures, and each row's counts, to those requests", async () => {
    await render();
    const failed = [...host.querySelectorAll("a")].find((a) => a.textContent?.includes("see them"))!;
    const url = new URL(failed.getAttribute("href")!, "http://x");
    expect(url.pathname).toBe("/logs");
    expect(url.searchParams.get("status")).toBe("error");
    // The range's UTC days, 00:00Z on the first to the end of the last.
    expect(new Date(Number(url.searchParams.get("since"))).toISOString()).toBe("2026-09-01T00:00:00.000Z");
    expect(new Date(Number(url.searchParams.get("until"))).toISOString()).toBe("2026-09-27T23:59:59.999Z");
    const row = [...host.querySelectorAll("tbody a")].find((a) => a.getAttribute("title") === "See the failed requests")!;
    expect(row.getAttribute("href")).toContain("team=t1");
    expect(row.getAttribute("href")).toContain("status=error");
  });

  it("splits the chart by team from the per-day rows", async () => {
    await render();
    expect(calls.some((c) => c.includes("group_by=day_tenant"))).toBe(true);
  });

  it("filters the table without rebuilding the chart", async () => {
    await render();
    const before = chartBuilds.count;
    const box = host.querySelector<HTMLInputElement>("input[aria-label='Filter rows']")!;
    const setValue = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    for (const text of ["v", "vo", "voy"]) {
      await act(async () => { setValue.call(box, text); box.dispatchEvent(new Event("input", { bubbles: true })); });
    }
    expect(chartBuilds.count).toBe(before);
    const rows = [...host.querySelectorAll("tbody tr")].map((r) => r.textContent ?? "");
    expect(rows.some((r) => r.includes("voyager"))).toBe(true);
    expect(rows.some((r) => r.includes("research-computing"))).toBe(false);
  });

  it("shows the first 300 breakdown rows until asked for all", async () => {
    const many = Array.from({ length: 350 }, (_, i) => row({ tenant_id: `t${i}`, cost_usd: 1 }));
    const real = globalThis.fetch;
    vi.stubGlobal("fetch", vi.fn(async (url: string) => {
      if (new URL(url, "http://x").searchParams.get("group_by") === "tenant") return { ok: true, json: async () => many } as Response;
      return real(url);
    }));
    await render();
    const bodyRows = () => host.querySelectorAll("tbody tr[class*='border-t']").length;
    const showAll = () => [...host.querySelectorAll("button")].find((b) => b.textContent?.startsWith("Show all 350"));
    expect(showAll()).toBeTruthy();
    expect(bodyRows()).toBe(301);
    await act(async () => showAll()!.click());
    expect(showAll()).toBeUndefined();
    expect(bodyRows()).toBe(350);
  });

  it("filters the whole page to a team clicked in the ranking", async () => {
    await render();
    const team = [...host.querySelectorAll("button")].find((b) => b.textContent?.startsWith("research-computing"))!;
    expect(team.textContent).toContain("75%");
    await act(async () => team.click());
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    expect(calls.some((c) => c.includes("tenant_id=t1"))).toBe(true);
    expect(host.textContent).toContain("· research-computing ·");
  });
});
