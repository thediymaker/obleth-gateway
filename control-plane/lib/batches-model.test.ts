import { describe, expect, it } from "vitest";
import { batchName, batchState, duration, etaSecs, filterCount, isActive, ownerOf, percentOf } from "@/lib/batches-model";
import type { AdminBatch } from "@/lib/obleth";

const NOW = Date.parse("2026-11-07T12:00:00Z");

function batch(over: Partial<AdminBatch> = {}): AdminBatch {
  return {
    id: "batch_1",
    tenant_id: "11111111-2222-3333-4444-555555555555",
    tenant_name: "research",
    key_id: "k",
    key_name: "classifier",
    model: "m",
    endpoint: "/v1/chat/completions",
    status: "in_progress",
    total: 1000,
    completed: 0,
    failed: 0,
    completion_window: "24h",
    created_at: "2026-11-07T11:00:00Z",
    in_progress_at: "2026-11-07T11:00:00Z",
    deadline_at: "2026-11-08T11:00:00Z",
    last_progress_at: null,
    finished_at: null,
    waiting: false,
    metadata: null,
    errors: null,
    ...over,
  };
}

describe("batches model", () => {
  it("counts progress from both outcomes", () => {
    expect(percentOf(batch({ completed: 240, failed: 10 }))).toBe(25);
    expect(percentOf(batch({ total: 0 }))).toBe(0);
    expect(percentOf(batch({ completed: 999 }))).toBe(99);
  });

  it("says durations in words", () => {
    expect(duration(45)).toBe("45 s");
    expect(duration(12 * 60)).toBe("12 min");
    expect(duration(3 * 3600 + 5 * 60)).toBe("3 h 5 min");
    expect(duration(2 * 86400)).toBe("2 d");
  });

  it("estimates the time left from the pace so far", () => {
    // 250 of 1000 in one hour: three more hours.
    expect(etaSecs(batch({ completed: 250 }), NOW)).toBeCloseTo(3 * 3600);
    expect(etaSecs(batch(), NOW)).toBeNull();
    expect(etaSecs(batch({ status: "completed", completed: 1000 }), NOW)).toBeNull();
  });

  it("describes each status in plain words", () => {
    expect(batchState(batch({ completed: 250 }), NOW)).toMatchObject({ label: "Running", detail: "About 3 h left at this pace" });
    const waiting = batchState(batch({ waiting: true, last_progress_at: "2026-11-07T11:50:00Z" }), NOW);
    expect(waiting.label).toBe("Waiting for capacity");
    expect(waiting.detail).toContain("10 min");
    expect(batchState(batch({ status: "completed", completed: 990, failed: 10 }), NOW)).toMatchObject({ label: "Done", tone: "problem", detail: "10 requests failed" });
    expect(batchState(batch({ status: "expired", completed: 600 }), NOW).detail).toBe("400 requests didn't run before the 24h deadline");
    expect(batchState(batch({ status: "failed", errors: { data: [{ line: 3, message: "missing custom_id" }] } }), NOW).detail).toBe("Line 3: missing custom_id");
  });

  it("names batches and owners", () => {
    expect(batchName(batch({ metadata: { name: " Abstracts " } }))).toBe("Abstracts");
    expect(batchName(batch())).toBeNull();
    expect(ownerOf(batch())).toBe("research · classifier");
    expect(ownerOf(batch({ tenant_name: null, key_name: null }))).toBe("11111111");
  });

  it("counts batches per filter", () => {
    const counts = { in_progress: 2, validating: 1, completed: 5, expired: 1 };
    expect(filterCount("active", counts)).toBe(3);
    expect(filterCount("completed", counts)).toBe(5);
    expect(filterCount("all", counts)).toBe(9);
    expect(isActive("cancelling")).toBe(true);
    expect(isActive("cancelled")).toBe(false);
  });
});
