import { describe, expect, it } from "vitest";
import { adviceLogsHref, adviceSource, fixLabel, judgeLine, snoozedLine, splitAdvice } from "@/lib/advice-model";
import type { AdviceReportView } from "@/lib/obleth";

const report = (over: Partial<AdviceReportView> = {}): AdviceReportView => ({
  generated_at_ms: 1_000_000, window_hours: 24, findings: [], snoozed: [], judge: {}, ...over,
});

describe("advice in words", () => {
  it("links a finding to exactly its requests", () => {
    expect(adviceLogsHref({ model: "m", key_id: "k1", status_code: 400 })).toBe("/logs?model=m&key=k1&code=400&window=24h");
  });

  it("names the fix by what it turns on", () => {
    expect(fixLabel({ kind: "grant_boon", boon: "vision", models: ["a"] })).toBe("Turn on the vision boon for a");
    expect(fixLabel({ kind: "grant_boon", boon: "vision", models: ["a", "b"] })).toBe("Turn on the vision boon for 2 models");
  });

  it("leaves out the fix question when there is no fix", () => {
    expect(judgeLine({ model: "j", hurting_users: 0.1, worth_acting: 0.2 })).toBe("j rates it: users affected 10%, worth acting on today 20%.");
  });

  it("folds notes away from what needs you", () => {
    const f = (key: string, severity: "act" | "watch" | "note") => ({ key, check: "x", severity, title: key, detail: "", evidence: [], models: [], weight: 1 });
    const { needs, notes } = splitAdvice(report({ findings: [f("a", "act"), f("b", "note"), f("c", "watch")] }));
    expect(needs.map((x) => x.key)).toEqual(["a", "c"]);
    expect(notes.map((x) => x.key)).toEqual(["b"]);
    expect(splitAdvice(null)).toEqual({ needs: [], notes: [] });
  });

  it("says where the numbers come from and why nothing was judged", () => {
    expect(adviceSource(report({ judge: { model: "j" } }), 1_000_000 + 120_000)).toBe("From the last 24 hours of requests, checked 2 min ago. Ordered by j.");
    expect(adviceSource(report({ judge: { model: "j", pending: true } }), 1_000_000)).toBe("From the last 24 hours of requests, checked just now. Ordered by j; it is still judging some findings.");
    expect(adviceSource(report({ judge: { error: "no verdicts model is set" } }), 1_000_000)).toBe("From the last 24 hours of requests, checked just now. Not judged: no verdicts model is set.");
  });

  it("tells a snooze from a dismissal", () => {
    expect(snoozedLine({ key: "k", title: "t", by: "a@b", at_ms: Date.UTC(2026, 10, 6, 12), until_ms: Date.UTC(2026, 10, 13, 12) })).toBe("Back on Nov 13 · set aside by a@b");
    expect(snoozedLine({ key: "k", title: "t", by: "", at_ms: Date.UTC(2026, 10, 6, 12) })).toBe("Dismissed on Nov 6");
  });
});
