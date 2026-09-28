import { describe, expect, it } from "vitest";
import { buildChart, change, daysBetween, eachDay, presetRange, previousRange, sortRows, totals } from "./reports-model";
import { timeline } from "./trace-model";
import type { SpanEntry, UsageDailyRow } from "./obleth";

const row = (over: Partial<UsageDailyRow> = {}): UsageDailyRow => ({
  day: "2026-09-01", tenant_id: "t1", key_id: "k1", model: "m1", requests: 10, success_requests: 9, error_requests: 1,
  input_tokens: 700, output_tokens: 300, total_tokens: 1000, estimated_tokens: 0, cache_hits: 1, cache_misses: 9,
  avg_ttft_ms: 400, avg_total_ms: 2000, cost_usd: 1, energy_wh: 10, energy_cost_usd: 0.1, co2_g: 3, ...over,
});

const sep27 = new Date(2026, 8, 27, 15, 0);

describe("date ranges", () => {
  it("counts presets back from today", () => {
    expect(presetRange("7d", sep27)).toEqual({ start: "2026-09-21", end: "2026-09-27" });
    expect(presetRange("month", sep27)).toEqual({ start: "2026-09-01", end: "2026-09-27" });
    expect(presetRange("last-month", sep27)).toEqual({ start: "2026-08-01", end: "2026-08-31" });
    expect(daysBetween(presetRange("30d", sep27))).toBe(30);
  });

  it("compares a month so far with the same days of the month before", () => {
    expect(previousRange({ start: "2026-09-01", end: "2026-09-27" }, "month")).toEqual({ start: "2026-08-01", end: "2026-08-27" });
    // March 1–31 so far compares with February, which has fewer days.
    expect(previousRange({ start: "2026-03-01", end: "2026-03-31" }, "month")).toEqual({ start: "2026-02-01", end: "2026-02-28" });
    expect(previousRange({ start: "2026-08-01", end: "2026-08-31" }, "last-month")).toEqual({ start: "2026-07-01", end: "2026-07-31" });
  });

  it("compares any other range with as many days just before it", () => {
    expect(previousRange({ start: "2026-09-21", end: "2026-09-27" }, "7d")).toEqual({ start: "2026-09-14", end: "2026-09-20" });
    expect(eachDay({ start: "2026-09-29", end: "2026-10-02" })).toEqual(["2026-09-29", "2026-09-30", "2026-10-01", "2026-10-02"]);
  });
});

describe("totals", () => {
  it("adds counts and weights first token by the requests that succeeded", () => {
    const t = totals([row(), row({ success_requests: 1, avg_ttft_ms: 1400, requests: 1, error_requests: 0 })]);
    expect(t.requests).toBe(11);
    expect(t.failed).toBe(1);
    expect(t.avgTtftMs).toBe(500);
  });

  it("words a change against the period before", () => {
    expect(change(118, 100, "before")).toBe("▲ 18% vs before");
    expect(change(95, 100, "before")).toBe("▼ 5.0% vs before");
    expect(change(5, 0, "before")).toBe("new since before");
    expect(change(5, null, "before")).toBeNull();
  });
});

describe("the chart", () => {
  const range = { start: "2026-09-01", end: "2026-09-03" };
  const days = [row({ day: "2026-09-01", requests: 5 }), row({ day: "2026-09-03", requests: 7 })];

  it("fills every day and lines up the previous period by position", () => {
    const prev = { range: { start: "2026-08-29", end: "2026-08-31" }, rows: [row({ day: "2026-08-30", requests: 4 })] };
    const { days: out, series } = buildChart(range, "requests", "none", days, prev, [], () => "");
    expect(series).toEqual([]);
    expect(out.map((d) => [d.day, d.value, d.previous])).toEqual([
      ["2026-09-01", 5, 0],
      ["2026-09-02", 0, 4],
      ["2026-09-03", 7, 0],
    ]);
  });

  it("splits into the four largest and the rest", () => {
    const split = ["a", "b", "c", "d", "e", "f"].map((m, i) => row({ day: "2026-09-02", model: m, cost_usd: 60 - i * 10 }));
    const { days: out, series } = buildChart(range, "spend", "model", days, null, split, (r) => r.model);
    expect(series.map((s) => s.label)).toEqual(["a", "b", "c", "d", "2 more"]);
    expect(out[1].parts).toEqual({ a: 60, b: 50, c: 40, d: 30, __rest: 30 });
  });

  it("never splits first token, whose parts would not add up", () => {
    expect(buildChart(range, "ttft", "tenant", days, null, days, () => "").series).toEqual([]);
  });
});

describe("the breakdown", () => {
  it("sorts by any column, then by name", () => {
    const rows = [row({ cost_usd: 2 }), row({ cost_usd: 5 }), row({ cost_usd: 2 })].map((r, i) => ({ ...r, label: ["b", "a", "c"][i] }));
    expect(sortRows(rows, "cost_usd", true).map((r) => r.label)).toEqual(["a", "b", "c"]);
    expect(sortRows(rows, "label", false).map((r) => r.label)).toEqual(["a", "b", "c"]);
  });
});

describe("a trace's timeline", () => {
  const span = (name: string, start: number, dur: number, parent = "proxy_request"): SpanEntry => ({ request_id: "r", span_name: name, parent_span: parent, start_ms: start, duration_ms: dur, status: "ok", attributes: "{}" });

  it("places each step on the request's own scale, children indented, loop rounds left out", () => {
    const { steps, totalMs } = timeline([
      span("proxy_request", 1000, 1000, ""),
      span("auth_resolve", 1000, 10),
      span("upstream", 1100, 900),
      span("boon:tool_loop", 1200, 500, "upstream"),
      span("boon:tool_loop:iter:1", 1200, 200, "boon:tool_loop"),
    ]);
    expect(totalMs).toBe(1000);
    expect(steps.map((s) => [s.node.span.span_name, s.depth, s.left])).toEqual([
      ["auth_resolve", 0, 0],
      ["upstream", 0, 10],
      ["boon:tool_loop", 1, 20],
    ]);
    expect(steps[1].width).toBe(90);
  });
});
