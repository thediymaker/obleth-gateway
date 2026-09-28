import { afterEach, describe, expect, it, vi } from "vitest";
import {
  bucketFor,
  bucketLabel,
  DEFAULT_LOG_FILTERS,
  describeRequest,
  fillHistogram,
  loadViews,
  logParams,
  parseSearch,
  storeViews,
  timeSplit,
  windowRange,
} from "./logs-model";
import type { SpanEntry, UsageLogEntry } from "./obleth";

const lookups = {
  tenants: [{ id: "t-cs", name: "cs-teaching" }],
  keys: [{ id: "k-1", name: "canvas-tutor", prefix: "sk-ct9", tenantId: "t-cs" }],
  models: ["glm-5-3", "kimi-k2-7-code"],
};

const row = (over: Partial<UsageLogEntry> = {}): UsageLogEntry => ({
  request_id: "7c1e40a2-0000-0000-0000-000000000000", ts_ms: 1_000, tenant_id: "t-cs", key_id: "k-1", model: "glm-5-3",
  request_type: "chat", session_id: "", session_id_source: "none", device_id: "", admission: "fast", status_code: 200,
  input_tokens: 1200, output_tokens: 300, total_tokens: 1500, queue_wait_ms: 0, ttft_ms: 400, total_ms: 2400,
  cache_status: "off", cost_usd: 0.002, energy_wh: 0, energy_cost_usd: 0, co2_g: 0,
  tenant_name: "cs-teaching", key_name: "canvas-tutor", key_prefix: "sk-ct9", has_trace: false, ...over,
});

const span = (span_name: string, attributes: object, over: Partial<SpanEntry> = {}): SpanEntry => ({
  request_id: "r", span_name, parent_span: "proxy_request", start_ms: 0, duration_ms: 1, status: "ok", attributes: JSON.stringify(attributes), ...over,
});

describe("the search box", () => {
  it("turns tokens into filters and keeps free text as a request id prefix", () => {
    expect(parseSearch("status:error model:GLM-5-3 team:cs-teaching 7c1e", lookups)).toEqual({
      patch: { status: "error", model: "glm-5-3", tenantId: "t-cs", requestId: "7c1e" },
      unknown: [],
    });
  });

  it("finds a key by prefix or name, and takes its team with it", () => {
    expect(parseSearch("key:sk-ct9", lookups).patch).toEqual({ keyId: "k-1", tenantId: "t-cs" });
    expect(parseSearch("key:canvas-tutor", lookups).patch).toEqual({ keyId: "k-1", tenantId: "t-cs" });
  });

  it("says which tokens it could not place", () => {
    expect(parseSearch("model:nope colour:red status:ok", lookups)).toEqual({ patch: { status: "success" }, unknown: ["model:nope", "colour:red"] });
  });

  it("takes a session as given", () => {
    expect(parseSearch("session:sess_8f2c", lookups).patch).toEqual({ sessionId: "sess_8f2c" });
  });
});

describe("the query both reads share", () => {
  it("sends only what is set, and an upper bound only when zoomed", () => {
    const p = logParams({ ...DEFAULT_LOG_FILTERS, status: "error", model: "glm-5-3" }, { since: 100, until: 200 }, { limit: 50 });
    expect(Object.fromEntries(p)).toEqual({ since_ms: "100", status: "error", model: "glm-5-3", limit: "50" });
    const zoomed = logParams({ ...DEFAULT_LOG_FILTERS, window: "custom", sinceMs: 100, untilMs: 200 }, { since: 100, until: 200 });
    expect(zoomed.get("until_ms")).toBe("200");
  });

  it("slides a preset window with the clock and keeps a zoomed one where it was", () => {
    expect(windowRange(DEFAULT_LOG_FILTERS, 10_000_000)).toEqual({ since: 10_000_000 - 3_600_000, until: 10_000_000 });
    expect(windowRange({ ...DEFAULT_LOG_FILTERS, window: "custom", sinceMs: 5, untilMs: 9 }, 10_000_000)).toEqual({ since: 5, until: 9 });
  });

  it("sizes a zoomed window's bars so it gets at most 120", () => {
    expect(bucketFor({ ...DEFAULT_LOG_FILTERS, window: "custom", sinceMs: 0, untilMs: 10 * 60_000 })).toBe(5_000);
    expect(bucketFor({ ...DEFAULT_LOG_FILTERS, window: "24h" })).toBe(1_800_000);
  });
});

describe("the histogram", () => {
  it("names a bar's width the way people say it", () => {
    expect(["15000", "60000", "1800000", "21600000", "86400000"].map((m) => bucketLabel(Number(m)))).toEqual(["15 s", "minute", "30 min", "6 h", "day"]);
  });

  it("fills the quiet buckets with zero, oldest first", () => {
    const out = fillHistogram([{ bucket_ms: 120_000, requests: 4, errors: 1 }], { since: 60_000, until: 180_000 }, 60_000);
    expect(out).toEqual([
      { bucket_ms: 60_000, requests: 0, errors: 0 },
      { bucket_ms: 120_000, requests: 4, errors: 1 },
      { bucket_ms: 180_000, requests: 0, errors: 0 },
    ]);
  });
});

describe("a request's time", () => {
  it("splits into the wait, up to the first token, and the rest", () => {
    expect(timeSplit({ queue_wait_ms: 100, ttft_ms: 400, total_ms: 2400 })).toEqual({ wait: 100, first: 300, rest: 2000, total: 2400 });
  });

  it("reads a reply with no first token as all rest after the wait", () => {
    expect(timeSplit({ queue_wait_ms: 50, ttft_ms: 0, total_ms: 300 })).toEqual({ wait: 50, first: 0, rest: 250, total: 300 });
  });
});

describe("what happened", () => {
  it("tells a streamed success from its first token and finish", () => {
    expect(describeRequest(row())).toBe("Admitted straight away. First token after 400 ms, finished at 2.4 s.");
  });

  it("names the upstream failure, how many endpoints it could try, and that nothing was billed", () => {
    const spans = [span("admission", { decision: "queued", queue_wait_ms: 180 }), span("upstream", { status: 502, targets: 2 })];
    expect(describeRequest(row({ status_code: 502, queue_wait_ms: 180, ttft_ms: 0, total_ms: 30_000, cost_usd: 0, output_tokens: 0, admission: "queued" }), spans)).toBe(
      "Waited 180 ms for a slot in the glm-5-3 pool. The upstream answered 502 (Bad gateway) after 30 s, with 2 endpoints to try. Nothing was billed.",
    );
  });

  it("puts a request turned away at admission on the gateway", () => {
    expect(describeRequest(row({ status_code: 429, admission: "rejected", cost_usd: 0, total_ms: 3 }))).toBe(
      "Turned away at admission. The gateway answered 429 (Too many requests) after 3 ms. Nothing was billed.",
    );
  });

  it("says when the cache answered, and which model auto picked", () => {
    const spans = [span("auto_route", { chosen: "kimi-k2-7-code" })];
    expect(describeRequest(row({ cache_status: "hit", total_ms: 12 }), spans)).toBe(
      "Admitted straight away. The auto router picked kimi-k2-7-code. Answered from the response cache, without calling the upstream.",
    );
  });
});

describe("saved views", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("round-trips through this browser, filling in filters added since", () => {
    storeViews([{ name: "Failed glm", filters: { ...DEFAULT_LOG_FILTERS, status: "error", model: "glm-5-3" } }]);
    const stored = JSON.parse(localStorage.getItem("obleth:request-logs:views")!);
    delete stored[0].filters.includeInternal;
    localStorage.setItem("obleth:request-logs:views", JSON.stringify(stored));
    expect(loadViews()).toEqual([{ name: "Failed glm", filters: { ...DEFAULT_LOG_FILTERS, status: "error", model: "glm-5-3" } }]);
  });

  it("reads broken or blocked storage as none saved", () => {
    localStorage.setItem("obleth:request-logs:views", "{not json");
    expect(loadViews()).toEqual([]);
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("blocked"); });
    expect(loadViews()).toEqual([]);
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("full"); });
    expect(storeViews([])).toBe(false);
  });
});
