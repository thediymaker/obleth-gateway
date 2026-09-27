// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  appendHistoryTail,
  buildHistoryChart,
  FairshareDashboard,
  fetchHistory,
  limitsNote,
  slotModeBadge,
  replicaShare,
  thinHistory,
  type FairshareLiveView,
  type TenantFairshareView,
} from "./fairshare-dashboard";
import type { FairshareHistoryView } from "@/lib/obleth";

const mocks = vi.hoisted(() => ({
  view: undefined as FairshareLiveView | undefined,
  history: undefined as FairshareHistoryView | undefined,
  tail: undefined as FairshareHistoryView | undefined,
  queryKeys: [] as string[][],
  routes: [] as Array<{ id: string; model_name: string; upstream_model: string; api_base: string; enabled: boolean; max_in_flight: number | null }>,
  isError: false,
  save: vi.fn(),
  cap: vi.fn(),
  invalidate: vi.fn(),
  replace: vi.fn(),
}));
vi.mock("@/app/actions", () => ({ setWeightAction: mocks.save, setTenantMaxInFlightAction: mocks.cap }));
vi.mock("next/navigation", () => ({ useRouter: () => ({ replace: mocks.replace, refresh: vi.fn() }) }));
vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: mocks.invalidate }),
  useQuery: ({ queryKey }: { queryKey: string[] }) => {
    mocks.queryKeys.push(queryKey);
    const data =
      queryKey[0] === "fairshare-live" ? mocks.view
      : queryKey[0] === "fairshare-history" ? mocks.history
      : queryKey[0] === "fairshare-history-tail" ? mocks.tail
      : queryKey[0] === "model-routes" ? mocks.routes
      : undefined;
    return { data, isError: mocks.isError, isFetching: false, isPending: false, isSuccess: data !== undefined, dataUpdatedAt: 1_700_000_000_000 };
  },
}));
vi.mock("recharts", async (original) => ({
  ...await original<typeof import("recharts")>(),
  ResponsiveContainer: () => <div />,
}));

const tenant = (id: string, overrides: Partial<TenantFairshareView> = {}): TenantFairshareView => ({
  tenant_id: id, name: id, fairshare_group: "research", weight: 100,
  in_flight: 2, expected_slots: 4, queued: 3, served_tokens: 80, share_score: 20,
  weight_share: 0.5, ...overrides,
});
const route = (name: string, max_in_flight: number | null = 16) => ({ id: name, model_name: name, upstream_model: name, api_base: "", enabled: true, max_in_flight });

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  mocks.isError = false;
  mocks.history = undefined;
  mocks.tail = undefined;
  mocks.queryKeys = [];
  mocks.routes = [route("llama")];
  mocks.save.mockReset().mockResolvedValue(undefined);
  mocks.cap.mockReset().mockResolvedValue({ ok: true });
  mocks.invalidate.mockReset().mockResolvedValue(undefined);
  mocks.replace.mockReset();
  const tenants = [
    tenant("below-share", { in_flight: 1, queued: 3, expected_slots: 6, share_score: 0.1 }),
    tenant("over-share", { in_flight: 12, queued: 0, expected_slots: 6, share_score: 2 }),
    tenant("idle", { in_flight: 0, queued: 0 }),
  ];
  mocks.view = {
    algorithm: "fairshare", max_in_flight: 16, global_in_flight: 13, global_queued: 3,
    groups: [{ name: "research", weight: 1, in_flight: 13, queued: 3, expected_slots: 16, slot_cap: 16, served_tokens: 200, share_score: 50, weight_share: 1 }],
    tenants,
    keys: [
      { key_id: "k1", tenant_id: "below-share", name: "alice", weight: 100, max_in_flight: null, in_flight: 1, queued: 2, served_tokens: 30, share_score: 0.3, weight_share: 0.25, expected_slots: 2 },
      { key_id: "k2", tenant_id: "below-share", name: "bob", weight: 300, max_in_flight: 1, in_flight: 0, queued: 1, served_tokens: 50, share_score: 0.16, weight_share: 0.75, expected_slots: 4 },
    ],
    pools: [{ model: "llama", cap: 16, in_flight: 13, queued: 3, borrowed: 6, groups: [], tenants, keys: [] }],
    model_in_flight: { llama: 13 },
    model_queued: { llama: 3 },
  };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => { act(() => root.unmount()); host.remove(); document.body.innerHTML = ""; });

async function render(initialPool = "all") { await act(async () => root.render(<FairshareDashboard tenantNames={{ a: "a", b: "b", c: "c" }} initialPool={initialPool} />)); }
function button(text: string, scope: ParentNode = document) {
  const result = [...scope.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.includes(text) || b.getAttribute("aria-label") === text);
  if (!result) throw new Error(`Missing button: ${text}`);
  return result;
}
async function click(text: string) { await act(async () => button(text).click()); }
const panel = () => document.querySelector('[aria-label="Tenant details"]');
async function type(label: string, value: string) {
  const input = document.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  return input;
}

describe("all pools", () => {
  it("says who is waiting, lists pools fullest first, and marks a full pool", async () => {
    mocks.routes = [route("quiet"), route("llama")];
    await render();
    expect(host.textContent).toContain("3 waiting");
    expect(host.textContent).toContain("1 tenant is waiting below their fair share");
    const pools = [...host.querySelectorAll('[aria-label="Model pools"] button')].map((b) => b.textContent ?? "");
    expect(pools[0]).toContain("llama");
    expect(pools[0]).toContain("Busy");
    expect(pools[1]).toContain("quiet");
    expect(host.textContent).toContain("Tenants right now");
    expect(host.querySelector('[aria-label="Tenants right now"]')!.textContent).not.toContain("idle");
    expect(host.querySelector('[aria-label="Waiting now"]')!.textContent).toContain("below-share");
  });

  it("says no one is waiting, rather than showing an empty queue", async () => {
    mocks.view = { ...mocks.view!, global_queued: 0, tenants: mocks.view!.tenants.map((t) => ({ ...t, queued: 0 })) };
    await render();
    expect(host.textContent).toContain("No one is waiting");
    expect(host.querySelector('[aria-label="Waiting now"]')).toBeNull();
  });

  it("keeps the explanation one hover away, not on the page", async () => {
    await render();
    expect(host.textContent).not.toContain("How fairshare decides.");
    await act(async () => button("How fairshare decides", host).click());
    expect(host.querySelector('[role="tooltip"]')!.textContent).toContain("borrowing");
  });

  it("opens a pool from the list and puts it in the URL", async () => {
    await render();
    await act(async () => button("llama", host.querySelector('[aria-label="Model pools"]')!).click());
    expect(mocks.replace).toHaveBeenCalledWith("/fairshare?pool=llama", { scroll: false });
    expect(host.querySelector("h1")!.textContent).toBe("llama");
  });

  it("shows loading and stale data explicitly", async () => {
    mocks.view = undefined;
    await render();
    expect(host.textContent).toContain("Loading scheduler");
    expect(host.textContent).not.toContain("No one is waiting");
    mocks.view = { algorithm: "fairshare", max_in_flight: 8, global_in_flight: 0, global_queued: 0, groups: [], tenants: [] };
    mocks.isError = true;
    await render();
    expect(host.querySelector('[role="alert"]')!.textContent).toContain("Showing the last snapshot");
  });
});

describe("gateway modes", () => {
  it("counts cluster-wide with shared slots, and says how limits hold in a hover", async () => {
    mocks.view = { ...mocks.view!, replicas: 3, mode: "shared", shared_slots: true, replica_aware: true, configured_max_in_flight: 20, cluster_in_flight: 12, global_in_flight: 5 };
    await render();
    expect(host.querySelector('[data-testid="slot-mode"]')!.textContent).toBe("Shared slots · 3 gateways");
    expect(host.textContent).toContain("12 requests running");
    expect(host.textContent).toContain("12 of 20 slots");
    await act(async () => button("How limits hold across gateways", host).click());
    expect(host.textContent).toContain("In flight 12 of 20 (cluster-wide, 3 gateways) · this gateway 5");
  });

  it("flags fallback with the one inverted badge", async () => {
    mocks.view = { ...mocks.view!, replicas: 3, mode: "fallback", shared_slots: true, replica_aware: true, configured_max_in_flight: 48 };
    await render();
    const badge = host.querySelector('[data-testid="slot-mode"]')!;
    expect(badge.textContent).toBe("Fallback · split");
    expect(badge.parentElement!.className).toContain("bg-foreground");
  });

  it("describes every mode in one line", () => {
    const base = mocks.view!;
    expect(limitsNote({ ...base, mode: "split", replicas: 2 })).toContain("Shared slots are off");
    expect(limitsNote({ ...base, mode: "local", replicas: 1 })).toContain("configured size");
    expect(limitsNote({ ...base, mode: "local", replicas: 2 })).toContain("full configured limits");
    expect(limitsNote({ ...base, mode: "fallback", replica_aware: false, replicas: 2 })).toContain("full configured limits");
    expect(limitsNote(base)).toBe("");
    expect(slotModeBadge({ ...base, mode: "local", replicas: 1 })).toBe("");
    expect(slotModeBadge({ ...base, mode: "split", replicas: 4 })).toBe("Split across 4 gateways");
  });

  it("computes a replica's share the way the gateway does", () => {
    expect(replicaShare(8, 3)).toBe(3);
    expect(replicaShare(1, 4)).toBe(1);
    expect(replicaShare(32, 1)).toBe(32);
    expect(replicaShare(10, 0)).toBe(10);
  });
});

describe("one pool", () => {
  it("orders tenants by who is next in line and explains a wait behind a (?)", async () => {
    await render("llama");
    const rows = [...host.querySelectorAll('[aria-label="Who holds the slots"] li')].map((li) => li.textContent ?? "");
    expect(rows[0]).toContain("below-share");
    expect(rows[0]).toContain("Next in line");
    expect(rows[1]).toContain("over-share");
    expect(rows[1]).toContain("6 above");
    expect(rows.join()).not.toContain("idle");
    await act(async () => button("Why below-share is waiting", host).click());
    const tip = host.querySelector('[role="tooltip"]')!.textContent ?? "";
    expect(tip).toContain("over-share holds 6 above its share");
    expect(tip).toContain("number 1 in line");
  });

  it("keeps what you can do behind the Full badge's (?)", async () => {
    mocks.view!.pools![0] = { ...mocks.view!.pools![0], in_flight: 16 };
    mocks.view!.model_in_flight = { llama: 16 };
    await render("llama");
    expect(host.textContent).toContain("Full · 3 waiting");
    expect(host.textContent).not.toContain("Grow the pool.");
    await act(async () => button("What you can do about a full pool", host).click());
    expect(host.querySelector('[role="tooltip"]')!.textContent).toContain("Grow the pool.");
  });

  it("opens a pool the gateway has not served yet, saying so", async () => {
    mocks.routes = [route("llama"), route("quiet", 8)];
    await render("quiet");
    expect(host.querySelector("h1")!.textContent).toBe("quiet");
    expect(host.textContent).toContain("Nothing has run or waited in this pool");
  });

  it("asks for history scoped to the open pool", async () => {
    mocks.history = { interval_ms: 2000, retention_ms: 3_600_000, oldest_ts_ms: null, points: [] };
    await render("llama");
    expect(host.textContent).toContain("No samples yet");
    expect(mocks.queryKeys).toContainEqual(["fairshare-history", "llama"]);
    expect(mocks.queryKeys).toContainEqual(["fairshare-history-tail", "llama"]);
  });
});

describe("tenant panel", () => {
  it("previews a weight change per pool, keeps the draft after a failed save, and refreshes after retry", async () => {
    await render();
    await act(async () => button("below-share", host.querySelector('[aria-label="Tenants right now"]')!).click());
    expect(panel()!.textContent).toContain("3 waiting below share");
    // Keys: least served against weight first.
    expect(panel()!.textContent!.indexOf("bob")).toBeLessThan(panel()!.textContent!.indexOf("alice"));
    const input = await type("Fairshare weight", "300");
    expect(panel()!.textContent).toContain("llama");
    expect(panel()!.textContent).toContain("over-share");
    mocks.save.mockRejectedValueOnce(new Error("offline"));
    await click("Apply weight");
    expect(mocks.save).toHaveBeenCalledWith("below-share", 300);
    expect(panel()!.textContent).toContain("Could not save weight: offline");
    expect(input.value).toBe("300");
    await click("Apply weight");
    expect(mocks.invalidate).toHaveBeenCalledWith({ queryKey: ["fairshare-live"] });
    expect(panel()!.textContent).toContain("Weight saved: 300.");
  });

  it("sets and clears the per-model cap", async () => {
    await render();
    await act(async () => button("below-share", host.querySelector('[aria-label="Tenants right now"]')!).click());
    await type("Per-model cap", "8");
    await click("Save cap");
    expect(mocks.cap).toHaveBeenCalledWith("below-share", 8);
    expect(panel()!.textContent).toContain("Cap saved: 8 per model.");
  });
});

describe("history", () => {
  it("restores the activity chart from gateway history and says how far back it reaches", async () => {
    mocks.history = {
      interval_ms: 2000, retention_ms: 3_600_000, oldest_ts_ms: 1_700_000_000_000,
      points: [
        { ts_ms: 1_700_000_000_000, in_flight: 3, queued: 1, groups: { research: 3 } },
        { ts_ms: 1_700_000_002_000, in_flight: 5, queued: 0, groups: { research: 4, teaching: 1 } },
      ],
    };
    await render();
    expect(host.textContent).toContain("History since");
    expect(host.textContent).not.toContain("No samples yet");
  });

  it("shows history is disabled instead of waiting forever", async () => {
    mocks.history = { interval_ms: 2000, retention_ms: 0, oldest_ts_ms: null, points: [] };
    await render();
    expect(host.textContent).toContain("History disabled (OBLETH_FAIRSHARE_HISTORY_SECS=0)");
  });

  it("projects history points into stacked group series", () => {
    const { history, groupKeys } = buildHistoryChart([
      { ts_ms: 1_700_000_000_000, in_flight: 3, queued: 1, groups: { research: 3 } },
      { ts_ms: 1_700_000_002_000, in_flight: 5, queued: 0, groups: { teaching: 1, research: 4 } },
    ]);
    expect(groupKeys.map((g) => g.name)).toEqual(["research", "teaching"]);
    expect(history[1]).toMatchObject({ queued: 0, "group:research": 4, "group:teaching": 1 });
    expect(history[0]["group:teaching"]).toBe(0);
    expect(typeof history[0].time).toBe("string");
  });

  it("appends only newer tail points and trims to the retention window", () => {
    const prev = [
      { ts_ms: 1_000, in_flight: 1, queued: 0, groups: {} },
      { ts_ms: 3_000, in_flight: 2, queued: 0, groups: {} },
    ];
    const next = appendHistoryTail(prev, [
      { ts_ms: 3_000, in_flight: 2, queued: 0, groups: {} },
      { ts_ms: 5_000, in_flight: 4, queued: 1, groups: {} },
    ], 3_000);
    expect(next.map((p) => p.ts_ms)).toEqual([3_000, 5_000]);
    expect(appendHistoryTail(prev, [], 10_000)).toBe(prev);
  });

  it("thins a long history to a bounded row count, keeping the last row", () => {
    const rows = Array.from({ length: 1_800 }, (_, i) => ({ i }));
    const thinned = thinHistory(rows, 600);
    expect(thinned.length).toBeLessThanOrEqual(600);
    expect(thinned[thinned.length - 1]).toBe(rows[rows.length - 1]);
  });

  it("returns the same array reference when already within budget", () => {
    const rows = Array.from({ length: 100 }, (_, i) => ({ i }));
    expect(thinHistory(rows, 600)).toBe(rows);
  });

  it("requests the history endpoint the hook actually calls", async () => {
    const originalFetch = globalThis.fetch;
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ interval_ms: 2000, retention_ms: 1, oldest_ts_ms: null, points: [] }),
    });
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      await fetchHistory("llama", 1234);
      expect(fetchMock).toHaveBeenCalledWith("/api/live/fairshare/history?model=llama&since_ms=1234");
      await fetchHistory(undefined);
      expect(fetchMock).toHaveBeenCalledWith("/api/live/fairshare/history");
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});
