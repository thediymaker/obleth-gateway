import { describe, expect, it } from "vitest";
import type { ModelSpeedPoint } from "@/lib/obleth";
import { bucketStarts, changeLabel, columnLabels, describeSpeed, detailPaths, shade, sortSpeedRows, speedBucket, speedRows, type SpeedCell } from "./speed-model";

const point = (over: Partial<ModelSpeedPoint> = {}): ModelSpeedPoint => ({
  bucket_ms: 0, model: "m", requests: 100, rated: 90, gen_tokens_per_sec: 50, slow_tokens_per_sec: 20,
  prompt_tokens_per_sec: 2000, p50_ttft_ms: 300, ...over,
});
const day = (d: number) => Date.UTC(2026, 9, d);
const cell = (d: number, tps: number, usual = 50): SpeedCell => ({ t: day(d), tps, slowTps: tps / 2, rated: 30, ratio: tps / usual });

describe("speedRows", () => {
  it("compares with the period before and flags a model that slowed down", () => {
    const rows = speedRows(
      [point({ model: "a", gen_tokens_per_sec: 30 }), point({ model: "b", gen_tokens_per_sec: 60 })],
      [point({ model: "a", gen_tokens_per_sec: 50 }), point({ model: "b", gen_tokens_per_sec: 55 })],
      [], [],
    );
    const a = rows.find((r) => r.model === "a")!;
    expect(a.change).toBeCloseTo(-0.4);
    expect(a.slowedDown).toBe(true);
    expect(rows.find((r) => r.model === "b")!.slowedDown).toBe(false);
  });

  it("leaves out models that produced no tokens and marks thin samples", () => {
    const rows = speedRows([point({ model: "embed", rated: 0 }), point({ model: "a", rated: 5 })], null, [], []);
    expect(rows.map((r) => r.model)).toEqual(["a"]);
    expect(rows[0].few).toBe(true);
    expect(rows[0].change).toBeNull();
  });

  it("does not call a thin sample slowed down", () => {
    const rows = speedRows([point({ rated: 5, gen_tokens_per_sec: 10 })], [point({ gen_tokens_per_sec: 50 })], [], []);
    expect(rows[0].change).toBeCloseTo(-0.8);
    expect(rows[0].slowedDown).toBe(false);
  });

  it("puts a square in every bucket, against the model's own median, with gaps as null", () => {
    const rows = speedRows([point()], null, [
      point({ bucket_ms: day(2), gen_tokens_per_sec: 25 }),
      point({ bucket_ms: day(1), gen_tokens_per_sec: 50 }),
      point({ bucket_ms: day(3), rated: 0 }),
    ], [day(1), day(2), day(3)]);
    expect(rows[0].cells.map((c) => c?.ratio ?? null)).toEqual([1, 0.5, null]);
  });
});

describe("sortSpeedRows", () => {
  const rows = speedRows(
    [point({ model: "a", gen_tokens_per_sec: 10, rated: 500 }), point({ model: "b", gen_tokens_per_sec: 90, rated: 50 }), point({ model: "c", gen_tokens_per_sec: 40, rated: 100 })],
    [point({ model: "a", gen_tokens_per_sec: 10 }), point({ model: "b", gen_tokens_per_sec: 100 })],
    [], [],
  );
  it("sorts each way", () => {
    expect(sortSpeedRows(rows, "busiest").map((r) => r.model)).toEqual(["a", "c", "b"]);
    expect(sortSpeedRows(rows, "fastest").map((r) => r.model)).toEqual(["b", "c", "a"]);
    expect(sortSpeedRows(rows, "slowest").map((r) => r.model)).toEqual(["a", "c", "b"]);
    // b slowed by 10%, a held steady, c has nothing to compare with.
    expect(sortSpeedRows(rows, "slowed").map((r) => r.model)).toEqual(["b", "a", "c"]);
  });
});

describe("buckets", () => {
  it("uses hours for a day or two and days beyond", () => {
    expect(speedBucket({ start: "2026-10-22", end: "2026-10-23" })).toBe("hour");
    expect(speedBucket({ start: "2026-10-17", end: "2026-10-23" })).toBe("day");
  });

  it("starts each day at 00:00 UTC and stops hours at now", () => {
    expect(bucketStarts({ start: "2026-10-21", end: "2026-10-23" }, "day")).toEqual([day(21), day(22), day(23)]);
    const now = Date.UTC(2026, 9, 23, 2, 30);
    expect(bucketStarts({ start: "2026-10-23", end: "2026-10-23" }, "hour", now)).toHaveLength(3);
  });

  it("labels a column where a month starts, and thins labels on long ranges", () => {
    const labels = columnLabels([Date.UTC(2026, 8, 30), day(1), day(2)], "day");
    expect(labels[1]).not.toBe("1");
    expect(labels[2]).toBe("2");
    const many = columnLabels(bucketStarts({ start: "2026-09-01", end: "2026-10-31" }, "day"), "day");
    expect(many.filter(Boolean).length).toBeLessThanOrEqual(16);
  });
});

describe("shade", () => {
  it("is dim at usual speed and brightest at half speed or worse", () => {
    expect(shade(1)).toBe(shade(1.4));
    expect(shade(0.5)).toBe(shade(0.2));
    expect(shade(0.5)).not.toBe(shade(1));
  });
});

describe("describeSpeed", () => {
  const row = (cells: (SpeedCell | null)[]) => speedRows([point()], null, [], []).map((r) => ({ ...r, cells }))[0];

  it("says when a model held steady", () => {
    expect(describeSpeed(row([cell(1, 50), cell(2, 47)]), "day")).toBe("Within 6% of its usual speed every day it was used.");
  });

  it("names the slow days, the low point, whether it recovered, and the gaps", () => {
    const text = describeSpeed(row([cell(1, 50), cell(2, 25), cell(3, 30), null, cell(5, 49)]), "day");
    expect(text).toMatch(/^Slower than usual on Oct 2 and Oct 3, as low as 50% of usual\./);
    expect(text).toContain("Back to usual by Oct 5.");
    expect(text).toContain("No requests on 1 day.");
  });

  it("says when it is still slow", () => {
    expect(describeSpeed(row([cell(1, 50), cell(2, 20)]), "day")).toContain("Still slow on Oct 2.");
  });
});

describe("detailPaths", () => {
  it("breaks the line at gaps and draws a lone day as a short stroke", () => {
    const p = detailPaths([cell(1, 50), cell(2, 40), null, cell(4, 30)], 50, 400, 100);
    expect(p.median.match(/M/g)).toHaveLength(2);
    expect(p.band.match(/Z/g)).toHaveLength(2);
    expect(p.top).toBe(60);
    expect(p.usual).toMatch(/^M0 [\d.]+ H400$/);
  });
});

it("labels a change", () => {
  expect(changeLabel(-0.324)).toBe("▼ 32%");
  expect(changeLabel(0.05)).toBe("▲ 5.0%");
  expect(changeLabel(0.001)).toBe("same");
  expect(changeLabel(null)).toBeNull();
});
