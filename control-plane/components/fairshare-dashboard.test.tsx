// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FairshareDashboard, type FairshareLiveView, type TenantFairshareView } from "./fairshare-dashboard";

const mocks = vi.hoisted(() => ({
  view: undefined as FairshareLiveView | undefined,
  isError: false,
  save: vi.fn(),
  invalidate: vi.fn(),
}));
vi.mock("@/app/actions", () => ({ setWeightAction: mocks.save }));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: mocks.invalidate }),
  useQuery: ({ queryKey }: { queryKey: string[] }) => ({
    data: queryKey[0] === "fairshare-live" ? mocks.view : [],
    isError: mocks.isError,
    isFetching: false,
    dataUpdatedAt: 1_700_000_000_000,
  }),
}));
vi.mock("recharts", async (original) => ({
  ...await original<typeof import("recharts")>(),
  ResponsiveContainer: () => <div />,
}));

const tenant = (id: string, overrides: Partial<TenantFairshareView> = {}): TenantFairshareView => ({
  tenant_id: id, name: id, fairshare_group: "research", weight: 4,
  in_flight: 2, expected_slots: 4, queued: 3, served_tokens: 80, share_score: 20,
  weight_share: 0.5, ...overrides,
});
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  mocks.isError = false;
  mocks.save.mockReset().mockResolvedValue(undefined);
  mocks.invalidate.mockReset().mockResolvedValue(undefined);
  mocks.view = {
    algorithm: "fairshare", max_in_flight: 16, global_in_flight: 7, global_queued: 10,
    groups: [{ name: "research", weight: 1, in_flight: 7, queued: 10, expected_slots: 8, slot_cap: 12, served_tokens: 200, share_score: 50, weight_share: 0.5 }],
    tenants: [tenant("over-share", { in_flight: 5, expected_slots: 3, queued: 7 }), tenant("below-share"), tenant("idle", { in_flight: 0, queued: 0 })],
  };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => { act(() => root.unmount()); host.remove(); });
async function render() { await act(async () => root.render(<FairshareDashboard tenantNames={{}} />)); }
function button(text: string) {
  const result = [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.includes(text) || b.getAttribute("aria-label") === text);
  if (!result) throw new Error(`Missing button: ${text}`);
  return result;
}
async function click(text: string) { await act(async () => button(text).click()); }
async function tab(text: string) {
  await act(async () => button(text).dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 })));
}
function inspector() { return host.querySelector('[aria-label="Tenant details"]')!; }

describe("fairshare operations", () => {
  it("prioritizes waiting below share, excluding idle tenants, and keeps selection across snapshots", async () => {
    await render();
    const waiting = [...host.querySelectorAll("li button")];
    expect(waiting.map((b) => b.textContent)).toEqual([expect.stringContaining("below-share"), expect.stringContaining("over-share")]);
    expect(inspector().textContent).toContain("below-share");
    await click("over-share");
    mocks.view = { ...mocks.view!, tenants: mocks.view!.tenants.map((t) => t.tenant_id === "over-share" ? { ...t, queued: 0 } : t) };
    await render();
    expect(inspector().textContent).toContain("over-share");
    expect(inspector().textContent).toContain("No queued requests");
  });

  it("uses the canonical whole-slot predicate for fractional expected shares", async () => {
    mocks.view!.tenants = [tenant("fractional", { in_flight: 2, expected_slots: 2.9 })];
    await render();
    expect(inspector().textContent).toContain("Waiting for admission");
    expect(inspector().textContent).not.toContain("slots below expected");
  });

  it("opens all waiting tenants and drills from group allocation into a filtered workbench", async () => {
    mocks.view!.tenants.push(tenant("other-group", { fairshare_group: "community" }));
    await render();
    await click("View all waiting");
    expect(host.querySelectorAll("tbody tr")).toHaveLength(3);
    expect(host.querySelector("table")!.textContent).not.toContain("idle");
    await tab("Allocation");
    await click("Inspect research tenants");
    expect(host.querySelector("table")!.textContent).toContain("idle");
    expect(host.querySelector("table")!.textContent).not.toContain("other-group");
  });

  it("shows loading and stale data explicitly, without claiming a clear scheduler", async () => {
    mocks.view = undefined;
    await render();
    expect(host.textContent).toContain("Loading scheduler");
    expect(host.textContent).not.toContain("Scheduler clear");
    mocks.view = { algorithm: "fairshare", max_in_flight: 8, global_in_flight: 0, global_queued: 0, groups: [], tenants: [] };
    mocks.isError = true;
    await render();
    expect(host.querySelector('[role="alert"]')!.textContent).toContain("Showing the last snapshot");
    expect(host.textContent).not.toContain("Scheduler clear");
  });

  it("keeps queued work visible at model capacity and labels unavailable caps honestly", async () => {
    mocks.view!.model_in_flight = { "new-model": 2 };
    mocks.view!.model_queued = { "new-model": 4 };
    mocks.view!.groups[0].in_flight = 14;
    await render();
    await tab("Allocation");
    expect(host.textContent).toContain("2 active / cap unavailable");
    expect(host.textContent).toContain("4 queued");
    expect(host.textContent).toContain("Borrowed2");
    expect(host.textContent).not.toContain("hard limits");
  });

  it("retains the weight draft after a failed save and refreshes after retry", async () => {
    await render();
    await click("Edit tenant weight");
    const input = host.querySelector<HTMLInputElement>('[aria-label="Fairshare weight"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "7");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    mocks.save.mockRejectedValueOnce(new Error("offline"));
    await click("Apply weight");
    expect(mocks.save).toHaveBeenCalledWith("below-share", 7);
    expect(inspector().textContent).toContain("Could not save weight");
    expect(input.value).toBe("7");
    await click("Apply weight");
    expect(mocks.invalidate).toHaveBeenCalledWith({ queryKey: ["fairshare-live"] });
    expect(host.querySelector('[aria-label="Fairshare weight"]')).toBeNull();
  });

  it("scopes every panel to one model pool and lists the selected tenant's keys", async () => {
    mocks.view = {
      ...mocks.view!,
      keys: [
        { key_id: "k1", tenant_id: "below-share", name: "alice", weight: 100, max_in_flight: null, in_flight: 1, queued: 2, served_tokens: 30, share_score: 0.3, weight_share: 0.25, expected_slots: 2 },
        { key_id: "k2", tenant_id: "below-share", name: "bob", weight: 300, max_in_flight: 1, in_flight: 1, queued: 1, served_tokens: 50, share_score: 0.16, weight_share: 0.75, expected_slots: 6 },
      ],
      pools: [
        { model: "llama", cap: 4, in_flight: 1, queued: 3, borrowed: 0, groups: mocks.view!.groups,
          tenants: [tenant("below-share", { in_flight: 1, queued: 3, expected_slots: 2 })],
          keys: [{ key_id: "k1", tenant_id: "below-share", name: "alice", weight: 100, max_in_flight: null, in_flight: 1, queued: 3, served_tokens: 30, share_score: 0.3, weight_share: 1, expected_slots: 4 }] },
        { model: "qwen", cap: 12, in_flight: 6, queued: 7, borrowed: 0, groups: mocks.view!.groups, tenants: mocks.view!.tenants, keys: [] },
      ],
    };
    await render();
    // Keys section under the selected tenant, sorted by share score.
    expect(inspector().textContent).toContain("bob");
    expect(inspector().textContent).toContain("alice");
    expect(inspector().textContent.indexOf("bob")).toBeLessThan(inspector().textContent.indexOf("alice"));
    // Switch scope to the llama pool.
    const select = host.querySelector<HTMLSelectElement>('select[aria-label="Model scope"]')!;
    await act(async () => { select.value = "llama"; select.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(host.textContent).toContain("1 / 4");
    expect(inspector().textContent).not.toContain("bob");
    expect([...host.querySelectorAll("li button")].map((b) => b.textContent)).toEqual([expect.stringContaining("below-share")]);
  });

  it("renders the default fixture, which is an older gateway's payload with no pools", async () => {
    await render();
    expect(host.querySelector('select[aria-label="Model scope"]')).toBeNull();
    expect(inspector().textContent).toContain("below-share");
  });
});
