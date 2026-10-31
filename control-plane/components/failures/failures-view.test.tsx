import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { FailuresView } from "./failures-view";
import { NIL_REQUEST_ID } from "@/lib/logs-model";
import type { UsageFailureGroup, UsageFailureGroups, UsageLogHistogram } from "@/lib/obleth";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

const now = Date.now();
const group = (over: Partial<UsageFailureGroup> = {}): UsageFailureGroup => ({
  error_class: "client_input", platform: false, tenant_id: "t-cs", tenant_name: "cs-teaching", key_id: "k-1", key_name: "canvas-tutor",
  model: "glm-5-3", status_code: 400, reason: "This model's maximum context length is 262144 tokens. However, you requested 300000 tokens.",
  requests: 300, first_ms: now - 3 * 3_600_000, last_ms: now - 4 * 60_000, ...over,
});

const data: UsageFailureGroups = {
  requests: 661_802,
  failures: 412,
  platform_failures: 87,
  groups: [
    group(),
    group({ error_class: "capacity", platform: true, status_code: 503, reason: "", requests: 87, key_id: NIL_REQUEST_ID, key_name: "", tenant_id: "t-rc", tenant_name: "rcusers" }),
    group({ error_class: "client_cancel", status_code: 499, reason: "", requests: 25, model: "kimi-k2-7-code" }),
  ],
};

const histogram: UsageLogHistogram = {
  bucket_ms: 1_800_000,
  buckets: [
    { bucket_ms: Math.floor((now - 3_600_000) / 1_800_000) * 1_800_000, requests: 900, errors: 40, platform_errors: 30 },
    { bucket_ms: Math.floor((now - 600_000) / 1_800_000) * 1_800_000, requests: 1200, errors: 12, platform_errors: 2 },
  ],
};

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;

function stubFetch(failures: UsageFailureGroups) {
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const body = url.startsWith("/api/live/usage/logs/histogram") ? histogram : failures;
    return { ok: true, json: async () => body } as Response;
  }));
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  stubFetch(data);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render(initial: UsageFailureGroups | null = data) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <FailuresView initialRange="24h" initial={initial} />
      </QueryClientProvider>,
    );
  });
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
}

const panel = (title: string) => host.querySelector(`section[aria-label="${title}"]`)!;
const rows = (title: string) => [...panel(title).querySelectorAll("li")];
const tile = (label: string) => [...host.querySelectorAll("a")].find((a) => a.textContent?.startsWith(label))!;

describe("the failures page", () => {
  it("says how many failed, and gives each side and the requests a tile linked to its requests", async () => {
    await render();
    expect(host.textContent).toContain("412 failed of 661,802 requests: 87 on the platform's side, 325 from callers.");
    expect(tile("Platform failures · 24h").getAttribute("href")).toBe("/logs?fault=platform&window=24h");
    expect(tile("Platform failures · 24h").textContent).toContain("87 of 661,802 requests");
    expect(tile("Caller failures · 24h").getAttribute("href")).toBe("/logs?fault=client&window=24h");
    expect(tile("Caller failures · 24h").textContent).toContain("325 of 661,802 requests");
    expect(tile("Requests · 24h").textContent).toContain("412 failed");
    expect(calls.some((c) => c.startsWith("/api/live/usage/logs/histogram?"))).toBe(true);
  });

  it("ranks each side's repeated failures in its own panel, each linked to its requests", async () => {
    await render();
    const platform = rows("On the platform's side");
    expect(platform).toHaveLength(1);
    // No key: the team instead, and the status where no reason was recorded.
    expect(platform[0].textContent).toContain("No capacity");
    expect(platform[0].textContent).toContain("rcusers");
    expect(platform[0].textContent).toContain("HTTP 503");
    expect(platform[0].querySelector("a")!.getAttribute("href")).toBe("/logs?code=503&class=capacity&model=glm-5-3&team=t-rc&window=24h");

    const [refused, hungUp] = rows("From callers");
    expect(rows("From callers")).toHaveLength(2);
    expect(refused.textContent).toContain("Request refused");
    expect(refused.textContent).toContain("glm-5-3");
    expect(refused.textContent).toContain("canvas-tutor");
    expect(refused.textContent).toContain("maximum context length is 262144 tokens");
    expect(refused.textContent).toContain("300");
    expect(refused.textContent).toContain("4m");
    expect(refused.querySelector("a")!.getAttribute("href")).toBe("/logs?code=400&class=client_input&model=glm-5-3&key=k-1&window=24h");
    expect(hungUp.textContent).toContain("Caller hung up");
  });

  it("reads another range when asked", async () => {
    await render();
    const week = [...host.querySelectorAll('[aria-label="Time range"] button')].find((b) => b.textContent === "7d") as HTMLButtonElement;
    await act(async () => week.click());
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
    const asked = calls.filter((c) => c.startsWith("/api/live/usage/failures?")).at(-1)!;
    const since = Number(new URL(asked, "http://x").searchParams.get("since_ms"));
    expect(Math.abs(Date.now() - 7 * 86_400_000 - since)).toBeLessThan(60_000);
  });

  it("says so when nothing failed", async () => {
    const quiet = { requests: 1200, failures: 0, platform_failures: 0, groups: [] };
    stubFetch(quiet);
    await render(quiet);
    expect(host.textContent).toContain("None of 1,200 requests failed.");
    expect(host.textContent).toContain("Nothing failed on the platform's side in the last 24 hours.");
    expect(host.textContent).toContain("No caller's request failed in the last 24 hours.");
  });
});
