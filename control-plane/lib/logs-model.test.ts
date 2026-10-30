import { afterEach, describe, expect, it, vi } from "vitest";
import {
  effortText,
  bucketFor,
  bucketLabel,
  DEFAULT_LOG_FILTERS,
  describeRequest,
  failuresOnly,
  fillHistogram,
  globTest,
  helperCallsParams,
  listLabel,
  helperPurpose,
  loadViews,
  NIL_REQUEST_ID,
  logParams,
  parseSearch,
  pickOne,
  servedRequest,
  storeViews,
  timeSplit,
  typeOptions,
  windowRange,
  withPatterns,
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
      patch: { status: "error", model: "glm-5-3", tenantId: "t-cs", requestId: "7c1e", patterns: {} },
      unknown: [],
      tooBroad: [],
    });
  });

  it("finds a key by prefix or name, and takes its team with it", () => {
    expect(parseSearch("key:sk-ct9", lookups).patch).toEqual({ keyId: "k-1", tenantId: "t-cs", patterns: {} });
    expect(parseSearch("key:canvas-tutor", lookups).patch).toEqual({ keyId: "k-1", tenantId: "t-cs", patterns: {} });
  });

  it("says which tokens it could not place", () => {
    expect(parseSearch("model:nope colour:red status:ok", lookups)).toEqual({ patch: { status: "success" }, unknown: ["model:nope", "colour:red"], tooBroad: [] });
  });

  it("reads a status code as that exact status", () => {
    expect(parseSearch("status:502", lookups).patch).toEqual({ statusCode: "502" });
  });

  it("takes a session as given", () => {
    expect(parseSearch("session:sess_8f2c", lookups).patch).toEqual({ sessionId: "sess_8f2c" });
  });

  const many = {
    tenants: [{ id: "t-rc", name: "rcusers" }, { id: "t-cs", name: "cs-teaching" }],
    keys: [
      { id: "k-a", name: "jlee379-thisisatest", prefix: "sk_ac0c", tenantId: "t-rc" },
      { id: "k-b", name: "jlee379-cli", prefix: "sk_9f1e", tenantId: "t-cs" },
      { id: "k-c", name: "canvas-tutor", prefix: "sk-ct9", tenantId: "t-cs" },
    ],
    models: ["glm-5-3", "glm-5-3-spec", "kimi-k2-7-code"],
  };

  it("matches every key a * pattern fits, across teams", () => {
    const want = { keyId: "k-a,k-b", tenantId: "", patterns: { keyId: "jlee379*" } };
    expect(parseSearch("key:jlee379*", many).patch).toEqual(want);
    // Case-insensitive; the label keeps the pattern as typed.
    expect(parseSearch("JLEE379*", many).patch).toEqual({ ...want, patterns: { keyId: "JLEE379*" } });
    // A bare word that cannot be a request id matches names starting with it.
    expect(parseSearch("jlee379", many).patch).toEqual(want);
  });

  it("keeps a key's team when every match shares it", () => {
    expect(parseSearch("key:*-cli", many).patch).toEqual({ keyId: "k-b", tenantId: "t-cs", patterns: {} });
  });

  it("prefers an exact name for a bare word, then a team, then a model", () => {
    expect(parseSearch("canvas-tutor", many).patch).toMatchObject({ keyId: "k-c" });
    expect(parseSearch("rcusers", many).patch).toMatchObject({ tenantId: "t-rc" });
    expect(parseSearch("glm-5-3", many).patch).toMatchObject({ model: "glm-5-3" });
    expect(parseSearch("model:glm-5-3*", many).patch).toEqual({ model: "glm-5-3,glm-5-3-spec", patterns: { model: "glm-5-3*" } });
  });

  it("still reads hex as a request id prefix", () => {
    expect(parseSearch("7c1e40a2", many).patch).toEqual({ requestId: "7c1e40a2" });
  });

  it("refuses a pattern that matches too much, and one that matches nothing", () => {
    const keys = Array.from({ length: 101 }, (_, i) => ({ id: `k${i}`, name: `bulk-${i}`, prefix: `sk_${i}`, tenantId: "t" }));
    expect(parseSearch("key:bulk-*", { ...many, keys })).toEqual({ patch: {}, unknown: [], tooBroad: ["key:bulk-*"] });
    expect(parseSearch("nobody*", many).unknown).toEqual(["nobody*"]);
  });

  it("reads whose failures and which kinds", () => {
    expect(parseSearch("fault:platform", lookups).patch).toEqual({ fault: "platform" });
    expect(parseSearch("fault:caller class:client_input,limit", lookups).patch).toEqual({ fault: "client", errorClass: "client_input,limit" });
    expect(parseSearch("class:upstream-timeout", lookups).patch).toEqual({ errorClass: "upstream_timeout" });
    expect(parseSearch("fault:everyone class:sunspots", lookups).unknown).toEqual(["fault:everyone", "class:sunspots"]);
  });

  it("takes any type the gateway records", () => {
    expect(parseSearch("type:verdict", lookups).patch).toEqual({ requestType: "verdict" });
    expect(parseSearch("type:Some_New_Thing", lookups).patch).toEqual({ requestType: "some_new_thing" });
    expect(parseSearch("type:no-dashes", lookups).unknown).toEqual(["type:no-dashes"]);
  });
});

describe("a * search's list", () => {
  it("treats only * as special", () => {
    expect(globTest("a.b*")("a.bc")).toBe(true);
    expect(globTest("a.b*")("axbc")).toBe(false);
    expect(globTest("*(x)")("call(x)")).toBe(true);
  });

  it("reads in its menu as the pattern and a count, and gives way to a single pick", () => {
    const f = { ...DEFAULT_LOG_FILTERS, keyId: "k-a,k-b", model: "glm-5-3,glm-5-3-spec", patterns: { keyId: "jlee379*", model: "glm-5-3*" } };
    expect(listLabel(f, "keyId", "keys")).toBe("jlee379* · 2 keys");
    expect(listLabel({ ...f, patterns: {} }, "keyId", "keys")).toBe("2 keys");
    expect(listLabel(f, "tenantId", "teams")).toBeNull();
    const picked = { ...f, ...pickOne(f, "keyId", "k-a") };
    expect(listLabel(picked, "keyId", "keys")).toBeNull();
    expect(picked.patterns?.model).toBe("glm-5-3*");
  });

  it("merges a search's patterns over the ones in place", () => {
    const f = { ...DEFAULT_LOG_FILTERS, model: "a,b", patterns: { model: "*" } };
    const p = withPatterns(f, parseSearch("key:jlee379*", many2()).patch);
    expect(p.patterns).toEqual({ model: "*", keyId: "jlee379*", tenantId: undefined });
  });
});

function many2() {
  return {
    tenants: [],
    keys: [
      { id: "k-a", name: "jlee379-a", prefix: "sk_1", tenantId: "t1" },
      { id: "k-b", name: "jlee379-b", prefix: "sk_2", tenantId: "t2" },
    ],
    models: [],
  };
}

describe("the Type menu", () => {
  it("lists the known types, then any others the log has shown", () => {
    const opts = typeOptions(["chat", "guardrails_boon"], "brand_new");
    expect(opts).toContain("verdict");
    expect(opts).toContain("messages");
    expect(opts.slice(-2)).toEqual(["guardrails_boon", "brand_new"]);
    expect(opts.filter((t) => t === "chat")).toHaveLength(1);
  });
});

describe("the query both reads share", () => {
  it("sends only what is set, and an upper bound only when zoomed", () => {
    const p = logParams({ ...DEFAULT_LOG_FILTERS, status: "error", model: "glm-5-3" }, { since: 100, until: 200 }, { limit: 50 });
    expect(Object.fromEntries(p)).toEqual({ since_ms: "100", status: "error", model: "glm-5-3", limit: "50" });
    const zoomed = logParams({ ...DEFAULT_LOG_FILTERS, window: "custom", sinceMs: 100, untilMs: 200 }, { since: 100, until: 200 });
    expect(zoomed.get("until_ms")).toBe("200");
  });

  it("sends whose failures and which kinds, and counts either as looking at failures", () => {
    const f = { ...DEFAULT_LOG_FILTERS, fault: "platform" as const, errorClass: "capacity,network" };
    expect(Object.fromEntries(logParams(f, { since: 100, until: 200 }))).toEqual({ since_ms: "100", fault: "platform", error_class: "capacity,network" });
    expect(failuresOnly(f)).toBe(true);
    expect(failuresOnly({ ...DEFAULT_LOG_FILTERS, errorClass: "limit" })).toBe(true);
    expect(failuresOnly({ ...DEFAULT_LOG_FILTERS, statusCode: "200" })).toBe(false);
    expect(failuresOnly(DEFAULT_LOG_FILTERS)).toBe(false);
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

describe("helper calls", () => {
  it("says what each helper call was for, and any other type as recorded", () => {
    expect(helperPurpose("speculation_draft")).toBe("speculation draft");
    expect(helperPurpose("speculation_verify")).toBe("draft check");
    expect(helperPurpose("vision_boon")).toBe("image description");
    expect(helperPurpose("tool_loop")).toBe("tool loop turn");
    expect(helperPurpose("benchmark")).toBe("benchmark");
    expect(helperPurpose("chat")).toBe("chat");
    expect(helperPurpose("")).toBe("other");
  });

  it("reads the nil id, or none at all, as a request a client made", () => {
    expect(servedRequest(row({ parent_request_id: "9a2b0000-1111-2222-3333-444455556666" }))).toBe("9a2b0000-1111-2222-3333-444455556666");
    expect(servedRequest(row({ parent_request_id: NIL_REQUEST_ID }))).toBeNull();
    expect(servedRequest(row())).toBeNull();
  });

  it("looks an hour either side of when the request ended, health checks and benchmarks included", () => {
    expect(Object.fromEntries(helperCallsParams(row({ ts_ms: 10_000_000 })))).toEqual({
      parent_request_id: "7c1e40a2-0000-0000-0000-000000000000",
      since_ms: String(10_000_000 - 3_600_000),
      until_ms: String(10_000_000 + 3_600_000),
      include_internal: "true",
      limit: "200",
    });
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

describe("the reasoning effort line", () => {
  it("says what was asked and what the model got", () => {
    expect(effortText({ reasoning_effort: "", reasoning_effort_requested: "" })).toBeNull();
    expect(effortText({})).toBeNull();
    expect(effortText({ reasoning_effort: "high", reasoning_effort_requested: "high" })).toBe("Asked for high, served at high.");
    expect(effortText({ reasoning_effort: "low", reasoning_effort_requested: "none" })).toBe("Asked for none, served as low: this model has no none level.");
    expect(effortText({ reasoning_effort: "unsupported", reasoning_effort_requested: "medium" })).toMatch(/no effort control/);
    expect(effortText({ reasoning_effort: "", reasoning_effort_requested: "medium" })).toMatch(/as asked/);
    expect(effortText({ reasoning_effort: "none", reasoning_effort_requested: "" })).toMatch(/default for this model/);
  });
});
