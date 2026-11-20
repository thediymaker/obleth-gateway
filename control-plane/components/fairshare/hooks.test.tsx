import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FAIRSHARE_LIST_POLL_MS, FAIRSHARE_POLL_MS, useFairshareLive } from "./hooks";

const view = { algorithm: "drf", max_in_flight: 8, global_in_flight: 1, global_queued: 0, tenants: [{ tenant_id: "t1", expected_slots: 4, queued: 0 }, { tenant_id: "t2", expected_slots: 2, queued: 1 }], keys: [] };

let roots: Root[] = [];
let containers: HTMLElement[] = [];

/** Mounts a component that calls the hook with `options` and reports each result. */
async function mount<T>(client: QueryClient, options: Parameters<typeof useFairshareLive<T>>[0], seen: (data: T | undefined) => void) {
  function Probe() {
    const query = useFairshareLive<T>(options);
    seen(query.data);
    return null;
  }
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  containers.push(container);
  roots.push(root);
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <Probe />
      </QueryClientProvider>,
    );
  });
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(view), { status: 200 })));
});
afterEach(async () => {
  for (const root of roots) await act(async () => root.unmount());
  for (const container of containers) container.remove();
  roots = [];
  containers = [];
  vi.unstubAllGlobals();
});

describe("the fairshare live view", () => {
  it("polls slowly for a load column, and every two seconds only when a Fairshare page asks", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const intervals = () => client.getQueryCache().find({ queryKey: ["fairshare-live"] })?.observers.map((o) => o.options.refetchInterval);
    await mount(client, {}, () => {});
    expect(intervals()).toEqual([FAIRSHARE_LIST_POLL_MS]);
    await mount(client, { intervalMs: FAIRSHARE_POLL_MS }, () => {});
    expect(intervals()).toEqual([FAIRSHARE_LIST_POLL_MS, FAIRSHARE_POLL_MS]);
    expect(FAIRSHARE_POLL_MS).toBe(2000);
    expect(FAIRSHARE_LIST_POLL_MS).toBeGreaterThanOrEqual(10_000);
  });

  it("hands a page only the row it selects", async () => {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    let latest: unknown;
    await mount(client, { select: (v) => v.tenants.find((t) => t.tenant_id === "t2") ?? null }, (data) => (latest = data));
    await vi.waitFor(() => expect(latest).toEqual({ tenant_id: "t2", expected_slots: 2, queued: 1 }));
  });
});
