import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FairshareLiveView, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import type { OverviewWindow } from "@/lib/overview-data";
import { EMPTY_OVERVIEW_SUMMARY } from "@/lib/overview-summary";
import { OverviewDashboard } from "./overview-dashboard";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

let root: Root;
let host: HTMLDivElement;

const models = [
  { id: "a", model_name: "alpha", upstream_model: "alpha", api_base: "", model_type: "chat", enabled: true, max_in_flight: 8, tags: [] },
  { id: "b", model_name: "beta", upstream_model: "beta", api_base: "", model_type: "chat", enabled: true, max_in_flight: 8, tags: [] },
] as unknown as ModelRoute[];

const window: OverviewWindow = {
  range: "24h",
  now: Date.now(),
  summary: { ...EMPTY_OVERVIEW_SUMMARY, detailed: true, classified: true, requests: 1200, tokens: 50_000, errors: 6, platformErrors: 2, cost: 3.2, hasPricing: true, activeTenants: 2, p50TtftMs: 380, avgTtftMs: 520 },
  previous: { requests: 1000, tokens: 40_000, errors: 5, platformErrors: 1, cost: 2 },
  series: [],
  models: [],
  tenants: [{ id: "t1", name: "research", requests: 900, tokens: 40_000, cost: 2.5 }],
  otherTenants: { count: 0, requests: 0, tokens: 0, cost: 0 },
  cache: { hits: 20, misses: 80, tokens_saved: 1000 },
};

const fairshare = { algorithm: "drr", max_in_flight: 16, global_in_flight: 3, global_queued: 0, groups: [], tenants: [], model_in_flight: { alpha: 3 } } as unknown as FairshareLiveView;

function render(health: ModelHealthSummary[], view: FairshareLiveView = fairshare) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, refetchInterval: false, staleTime: Infinity } } });
  act(() => {
    root.render(
      <QueryClientProvider client={client}>
        <OverviewDashboard initialModels={models} initialWindow={window} initialContext={{ audit: [], readiness: null, tenantNames: {} }} initialHealth={health} initialFairshare={view} />
      </QueryClientProvider>,
    );
  });
}

const healthy = (id: string, name: string) => ({ model_id: id, model_name: name, status: "healthy", consecutive_failures: 0, maintenance_until: null }) as unknown as ModelHealthSummary;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("fetch", vi.fn(async () => new Response("[]", { status: 200 })));
  localStorage.clear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("OverviewDashboard", () => {
  it("says all is well once, in the header, and shows the window's numbers against the one before", () => {
    render([healthy("a", "alpha"), healthy("b", "beta")]);
    const text = host.textContent ?? "";
    expect(text).toContain("All systems normal");
    expect(text).toContain("2 of 2 models serving");
    expect(text).toContain("nothing queued");
    expect(text).not.toContain("Needs attention");
    expect(text).toContain("▲ 20% vs yesterday");
    expect(text).toContain("0.17%"); // 2 of 1,200 failed on the platform's side
    expect(text).toContain("380 ms");
    expect(text).toContain("$3.20");
    expect(text).toContain("20.0% hits");
    expect(text).toContain("Response cache · 24h");
  });

  it("heads the failures tile with the platform's failures, callers' underneath, each linked to its requests", () => {
    render([healthy("a", "alpha"), healthy("b", "beta")]);
    const tile = [...host.querySelectorAll("a")].find((a) => a.getAttribute("href") === "/failures?range=24h")!.parentElement!.parentElement!;
    expect(tile.textContent).toContain("Platform failures · 24h");
    const links = [...tile.querySelectorAll("a")].map((a) => [a.textContent, a.getAttribute("href")]);
    expect(links).toEqual([
      ["Platform failures · 24h", "/failures?range=24h"],
      ["0.17%", "/logs?fault=platform&window=24h"],
      ["4 more from callers", "/logs?fault=client&window=24h"],
    ]);
  });

  it("shows every failure as the error rate on a gateway that does not split them by side", () => {
    window.summary = { ...window.summary, classified: false };
    render([healthy("a", "alpha"), healthy("b", "beta")]);
    expect(host.textContent).toContain("Error rate · 24h");
    expect(host.textContent).toContain("0.50%"); // 6 failed of 1,200
    expect([...host.querySelectorAll("a")].some((a) => a.getAttribute("href") === "/logs?status=error&window=24h")).toBe(true);
    window.summary = { ...window.summary, classified: true };
  });

  it("says when the gateway is too old to report errors, rather than showing zero", () => {
    window.summary = { ...window.summary, detailed: false };
    render([healthy("a", "alpha"), healthy("b", "beta")]);
    expect(host.textContent).toContain("Needs a newer gateway");
    window.summary = { ...window.summary, detailed: true };
  });

  it("counts a tenant below share once, by the pool it is short in, with every gateway counted", () => {
    const row = (o: object) => ({ tenant_id: "t1", name: "research", fairshare_group: "default", weight: 100, served_tokens: 0, share_score: 0, weight_share: 0.5, ...o });
    const view = {
      ...fairshare, mode: "shared", scope: "cluster", replicas: 3, replicas_reporting: 3, cluster_in_flight: 16, configured_max_in_flight: 16,
      global_in_flight: 16, global_queued: 2, replica_in_flight: 3, replica_queued: 0,
      tenants: [row({ in_flight: 9, queued: 2, expected_slots: 8 }), row({ tenant_id: "t2", name: "ops", in_flight: 7, queued: 0, expected_slots: 4 })],
      pools: [
        { model: "alpha", cap: 8, configured_cap: 8, in_flight: 8, cluster_in_flight: 8, queued: 0, borrowed: 0, groups: [], keys: [], tenants: [row({ in_flight: 8, queued: 0, expected_slots: 4 })] },
        { model: "beta", cap: 8, configured_cap: 8, in_flight: 8, cluster_in_flight: 8, queued: 2, borrowed: 0, groups: [], keys: [], tenants: [row({ in_flight: 1, queued: 2, expected_slots: 4 }), row({ tenant_id: "t2", name: "ops", in_flight: 7, queued: 0, expected_slots: 4 })] },
      ],
    } as unknown as FairshareLiveView;
    render([healthy("a", "alpha"), healthy("b", "beta")], view);
    const text = host.textContent ?? "";
    expect(text).toContain("research is waiting below its fair share on beta");
    expect(text).toContain("2 requests queued while it holds 1 slot against a fair share of 4");
    expect(text).toContain("Cluster-wide across 3 gateways");
    expect(text).not.toContain("waiting is this gateway's");
    const link = [...host.querySelectorAll("a")].find((a) => a.getAttribute("href") === "/fairshare?pool=beta");
    expect(link?.textContent).toBe("Open the pool");
  });

  it("puts a failing model at the top, and Hide for 1 hour tucks it away in this browser", () => {
    render([{ ...healthy("a", "alpha"), status: "unhealthy", consecutive_failures: 5, last_http_status: 503 } as ModelHealthSummary, healthy("b", "beta")]);
    expect(host.textContent).toContain("1 needs attention");
    expect(host.textContent).toContain("alpha is failing health checks");
    expect(host.textContent).toContain("5 failed checks in a row · last check HTTP 503");

    const hide = [...host.querySelectorAll("button")].find((b) => b.textContent === "Hide for 1 hour")!;
    act(() => hide.click());
    expect(host.textContent).not.toContain("alpha is failing health checks");
    expect(host.textContent).toContain("1 attention item hidden");
    expect(JSON.parse(localStorage.getItem("obleth-overview:hidden") ?? "{}")).toHaveProperty(["down:alpha"]);
  });
});
