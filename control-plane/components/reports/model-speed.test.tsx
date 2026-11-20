import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ModelSpeed } from "./model-speed";
import type { ModelSpeedPoint } from "@/lib/obleth";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

const point = (over: Partial<ModelSpeedPoint>): ModelSpeedPoint => ({
  bucket_ms: 0, model: "m", requests: 100, rated: 90, gen_tokens_per_sec: 50, slow_tokens_per_sec: 20,
  prompt_tokens_per_sec: 2000, p50_ttft_ms: 300, ...over,
});

const calls: string[] = [];
let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  calls.length = 0;
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    calls.push(url);
    const p = new URL(url, "http://x").searchParams;
    const previous = Number(p.get("since_ms")) < Date.UTC(2026, 9, 1);
    const body = p.get("bucket") !== "none"
      ? [point({ bucket_ms: Date.UTC(2026, 9, 2), model: "glm-5-3", gen_tokens_per_sec: 30 })]
      : previous
        ? [point({ model: "glm-5-3", gen_tokens_per_sec: 60 }), point({ model: "muse", gen_tokens_per_sec: 80 })]
        : [point({ model: "glm-5-3", gen_tokens_per_sec: 30 }), point({ model: "muse", gen_tokens_per_sec: 82, rated: 300 }), point({ model: "embed", rated: 0 })];
    return { ok: true, json: async () => body } as Response;
  }));
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render(model = "", modelTypes: Record<string, string> = {}) {
  const onModel = vi.fn();
  await act(async () => {
    root.render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <ModelSpeed
          range={{ start: "2026-10-01", end: "2026-10-07" }}
          previous={{ start: "2026-09-24", end: "2026-09-30" }}
          filters={{ tenantId: "", keyId: "", model }}
          against="the previous period"
          retentionDays={180}
          modelTypes={modelTypes}
          onModel={onModel}
          logsFor={(extra) => `/logs?model=${extra?.model ?? ""}`}
        />
      </QueryClientProvider>,
    );
  });
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  return onModel;
}

describe("ModelSpeed", () => {
  it("asks for the range, the period before, and UTC days", async () => {
    await render();
    const buckets = calls.map((u) => new URL(u, "http://x").searchParams.get("bucket"));
    expect(buckets.sort()).toEqual(["day", "none", "none"]);
    const days = new URL(calls.find((u) => u.includes("bucket=day"))!, "http://x").searchParams;
    expect(days.get("tz")).toBeNull();
    expect(Number(days.get("since_ms"))).toBe(Date.UTC(2026, 9, 1));
    expect(Number(days.get("until_ms"))).toBe(Date.UTC(2026, 9, 8));
  });

  it("lists models that produced tokens, busiest first, and calls out one that slowed down", async () => {
    await render();
    const models = [...host.querySelectorAll("button[aria-expanded]")].map((b) => b.textContent ?? "");
    expect(models[0]).toContain("muse");
    expect(models[1]).toContain("glm-5-3");
    expect(models.join(" ")).not.toContain("embed");
    expect(host.textContent).toContain("▼ 50%");
    expect(host.textContent).toContain("1 slowed down by 20% or more");
    expect(host.textContent).toContain("Fastest muse · 82.0 tok/s");
  });

  it("leaves out models that are not chat models", async () => {
    await render("", { muse: "audio_speech" });
    expect(host.textContent).not.toContain("muse");
  });

  it("opens a model with its facts, its days and a way to report on it alone", async () => {
    const onModel = await render();
    const row = [...host.querySelectorAll("button[aria-expanded]")].find((b) => b.textContent!.includes("glm-5-3"))!;
    expect(row.getAttribute("aria-expanded")).toBe("false");
    await act(async () => { (row as HTMLElement).click(); });
    expect(row.getAttribute("aria-expanded")).toBe("true");
    expect(host.textContent).toContain("Down to the slowest tenth");
    expect(host.textContent).toContain("No requests on 6 days.");
    expect(host.querySelector('svg[role="img"]')).not.toBeNull();
    const only = [...host.querySelectorAll("button")].find((b) => b.textContent === "Report on this model only")!;
    await act(async () => { only.click(); });
    expect(onModel).toHaveBeenCalledWith("glm-5-3");
  });

  it("filters to one model and opens it", async () => {
    await render("glm-5-3");
    expect(calls.every((u) => u.includes("model=glm-5-3"))).toBe(true);
    expect(host.textContent).toContain("How fast glm-5-3 answered");
    expect(host.textContent).toContain("Down to the slowest tenth");
  });
});

describe("ModelSpeed when the gateway can't be read", () => {
  it("says it can't read the speed instead of no requests", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: false, status: 502, json: async () => ({}) }) as Response));
    await render();
    expect(host.textContent).toContain("Can't read the models' speed right now");
    expect(host.textContent).not.toContain("No token-producing requests");
  });
});
