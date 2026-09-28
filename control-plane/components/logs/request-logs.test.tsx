import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RequestLogs } from "./request-logs";
import type { UsageLogEntry } from "@/lib/obleth";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

const now = Date.now();
const entry = (over: Partial<UsageLogEntry> = {}): UsageLogEntry => ({
  request_id: "7c1e40a2-1111-2222-3333-444455556666", ts_ms: now - 12_000, tenant_id: "t-cs", key_id: "k-1", model: "glm-5-3",
  request_type: "chat", session_id: "sess_8f2c", session_id_source: "client", device_id: "", admission: "fast", status_code: 200,
  input_tokens: 1200, output_tokens: 300, total_tokens: 1500, queue_wait_ms: 0, ttft_ms: 400, total_ms: 2400,
  cache_status: "off", cost_usd: 0.002, energy_wh: 0, energy_cost_usd: 0, co2_g: 0,
  tenant_name: "cs-teaching", key_name: "canvas-tutor", key_prefix: "sk-ct9", has_trace: false, ...over,
});

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const body = url.includes("/histogram")
      ? { bucket_ms: 60_000, buckets: [{ bucket_ms: Math.floor((now - 60_000) / 60_000) * 60_000, requests: 40, errors: 2 }] }
      : url.includes("status=error")
        ? [entry({ request_id: "bad00000-0000-0000-0000-000000000000", status_code: 502, cost_usd: 0, output_tokens: 0, ttft_ms: 0 })]
        : [entry(), entry({ request_id: "abcd0000-0000-0000-0000-000000000000", model: "kimi-k2-7-code", status_code: 429, cost_usd: 0, total_ms: 3 })];
    return { ok: true, json: async () => body } as Response;
  }));
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  localStorage.clear();
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
        <RequestLogs
          tenants={[{ id: "t-cs", name: "cs-teaching" }]}
          models={["glm-5-3", "kimi-k2-7-code"]}
          keys={[{ id: "k-1", name: "canvas-tutor", prefix: "sk-ct9", tenantId: "t-cs" }]}
        />
      </QueryClientProvider>,
    );
  });
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
}

const type = (el: HTMLInputElement, value: string) => {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
};

describe("the request log", () => {
  it("counts the window from the histogram and lists the requests", async () => {
    await render();
    expect(host.textContent).toContain("40 requests in the last hour · 5.0% failed");
    const rows = host.querySelectorAll("tbody tr");
    expect(rows).toHaveLength(2);
    expect(rows[1].textContent).toContain("429");
    expect(calls.some((c) => c.startsWith("/api/live/usage/logs/histogram?"))).toBe(true);
  });

  it("applies what is typed in the search box as filters to both reads", async () => {
    await render();
    const box = host.querySelector<HTMLInputElement>('input[aria-label="Search requests"]')!;
    await act(async () => type(box, "status:error model:glm-5-3"));
    await act(async () => box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    const last = calls.filter((c) => c.includes("status=error"));
    expect(last.some((c) => c.startsWith("/api/live/usage/logs?") && c.includes("model=glm-5-3"))).toBe(true);
    expect(last.some((c) => c.startsWith("/api/live/usage/logs/histogram?"))).toBe(true);
    expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
    expect(box.value).toBe("");
  });

  it("says when a search token matches nothing", async () => {
    await render();
    const box = host.querySelector<HTMLInputElement>('input[aria-label="Search requests"]')!;
    await act(async () => type(box, "team:nobody"));
    await act(async () => box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(host.textContent).toContain("Didn't recognise team:nobody");
  });

  it("opens a request in the side panel with its account and numbers", async () => {
    await render();
    await act(async () => (host.querySelectorAll("tbody tr")[0] as HTMLElement).click());
    const panel = host.querySelector('[role="dialog"]')!;
    expect(panel.textContent).toContain("Answered in 2.4 s");
    expect(panel.textContent).toContain("First token after 400 ms, finished at 2.4 s.");
    expect(panel.textContent).toContain("canvas-tutor");
    // ↓ moves to the next request in the list.
    await act(async () => document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" })));
    expect(host.querySelector('[role="dialog"]')!.textContent).toContain("Too many requests");
  });

  it("filters to a request's session from the panel", async () => {
    await render();
    await act(async () => (host.querySelectorAll("tbody tr")[0] as HTMLElement).click());
    const button = [...host.querySelectorAll('[role="dialog"] button')].find((b) => b.textContent === "This session") as HTMLButtonElement;
    await act(async () => button.click());
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    expect(host.querySelector('[role="dialog"]')).toBeNull();
    expect(calls.some((c) => c.includes("session_id=sess_8f2c"))).toBe(true);
    expect(host.textContent).toContain("sess_8f2c");
  });

  it("pauses to page back through history", async () => {
    await render();
    const older = [...host.querySelectorAll("button")].find((b) => b.textContent === "That's all" || b.textContent === "Load older")!;
    // Two rows is under a page, so there is nothing older.
    expect(older.textContent).toBe("That's all");
    const live = [...host.querySelectorAll("button")].find((b) => b.textContent === "Live")!;
    await act(async () => live.click());
    expect([...host.querySelectorAll("button")].some((b) => b.textContent === "Paused")).toBe(true);
  });
});
