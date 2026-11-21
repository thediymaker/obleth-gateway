import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BatchesView } from "./batches-view";
import { cancelBatchAction } from "@/app/actions/batches";
import type { AdminBatch, AdminBatchList } from "@/lib/obleth";

vi.mock("next/navigation", () => ({ useRouter: () => ({ replace: vi.fn() }), usePathname: () => "/batches" }));
vi.mock("@/app/actions/batches", () => ({ cancelBatchAction: vi.fn(async () => ({ ok: true })) }));

const batch = (n: number): AdminBatch => ({
  id: `batch_${String(n).padStart(3, "0")}`, tenant_id: "t", tenant_name: "team", key_id: "k", key_name: "key", model: "m", endpoint: "/v1/chat/completions",
  status: "completed", total: 1, completed: 1, failed: 0, completion_window: "24h", created_at: "2026-11-01T00:00:00Z", in_progress_at: null,
  deadline_at: "2026-11-02T00:00:00Z", last_progress_at: null, finished_at: "2026-11-01T00:01:00Z", waiting: false, metadata: null, errors: null,
});
const page = (from: number, count: number, more: boolean): AdminBatchList => ({ data: Array.from({ length: count }, (_, i) => batch(from + i)), has_more: more, counts: { completed: 150 } });

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const body = url.includes("after=batch_099") ? page(100, 50, false) : page(0, 100, true);
    return { ok: true, json: async () => body } as Response;
  }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

it("reaches the older batches a page at a time and counts what it shows", async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => root.render(<QueryClientProvider client={client}><BatchesView initial={page(0, 100, true)} initialFilter="all" /></QueryClientProvider>));
  expect(host.textContent).toContain("100 batches shown");
  const more = [...host.querySelectorAll("button")].find((b) => b.textContent === "Load older")!;
  await act(async () => more.click());
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  expect(calls.some((u) => u.includes("after=batch_099"))).toBe(true);
  // Every batch is on the page now, so the footer has nothing left to offer.
  expect(host.textContent).toContain("batch_149");
  expect([...host.querySelectorAll("button")].some((b) => b.textContent === "Load older")).toBe(false);
});

it("asks before cancelling a batch, and does nothing when the answer is no", async () => {
  vi.mocked(cancelBatchAction).mockClear();
  const running: AdminBatch = { ...batch(1), status: "in_progress", total: 10, completed: 4, finished_at: null };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  await act(async () => root.render(<QueryClientProvider client={client}><BatchesView initial={{ data: [running], has_more: false, counts: { in_progress: 1 } }} initialFilter="all" /></QueryClientProvider>));
  const press = async (label: string) => {
    const b = [...document.querySelectorAll("button")].find((x) => x.textContent === label)!;
    await act(async () => { b.click(); await new Promise((r) => setTimeout(r, 0)); });
  };
  await press("Cancel…");
  expect(document.querySelector("[role=dialog]")?.textContent).toContain("6 not yet finished");
  await press("Cancel");
  expect(document.querySelector("[role=dialog]")).toBeNull();
  expect(cancelBatchAction).not.toHaveBeenCalled();
  await press("Cancel…");
  await press("Cancel the batch");
  expect(cancelBatchAction).toHaveBeenCalledWith(running.id);
});
