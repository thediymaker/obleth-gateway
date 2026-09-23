import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { describeResync, ResolverCacheCard } from "./settings-form";
import { resyncCacheAction } from "@/app/actions";

vi.mock("@/app/actions", () => ({ resyncCacheAction: vi.fn() }));

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.mocked(resyncCacheAction).mockReset();
});

const report = {
  keys: 12,
  keys_pruned: 1,
  models: 1,
  model_names_pruned: 0,
  mcp_servers: 2,
  mcp_servers_pruned: 0,
};

function button() {
  return [...host.querySelectorAll("button")].find((b) => b.textContent?.includes("Reconcile"))!;
}

it("reconciles on click and shows the returned counts", async () => {
  vi.mocked(resyncCacheAction).mockResolvedValue({ ok: true, report });
  act(() => root.render(<ResolverCacheCard />));
  await act(async () => {
    button().click();
  });
  expect(resyncCacheAction).toHaveBeenCalledTimes(1);
  expect(host.textContent).toContain(
    "Republished 12 keys, 1 model, and 2 MCP servers. Evicted 1 stale key, 0 stale model names, and 0 stale MCP servers.",
  );
});

it("shows the gateway error when the reconcile fails", async () => {
  vi.mocked(resyncCacheAction).mockResolvedValue({ ok: false, error: "cache sync failed" });
  act(() => root.render(<ResolverCacheCard />));
  await act(async () => {
    button().click();
  });
  expect(host.textContent).toContain("cache sync failed");
});

it("reports a clean reconcile without an eviction breakdown", () => {
  expect(describeResync({ ...report, keys_pruned: 0 })).toBe(
    "Republished 12 keys, 1 model, and 2 MCP servers. No stale entries found.",
  );
});
