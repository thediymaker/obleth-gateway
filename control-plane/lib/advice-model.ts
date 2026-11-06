import type { AdviceFindingView, AdviceFixView, AdviceJudgmentView, AdviceLogsView, AdviceReportView, SnoozedAdviceView } from "@/lib/obleth";
import { logsHref } from "@/lib/log-links";

const BOON_LABEL: Record<string, string> = {
  vision: "the vision boon",
  web_search: "web search",
  image_generation: "image generation",
};

/** The requests a finding is about, on the Request logs page. */
export function adviceLogsHref(logs: AdviceLogsView): string {
  return logsHref({
    model: logs.model,
    key: logs.key_id,
    code: logs.status_code,
    class: logs.error_class,
    window: "24h",
  });
}

/** What the Apply button does, in words. */
export function fixLabel(fix: AdviceFixView): string {
  const what = BOON_LABEL[fix.boon] ?? `the ${fix.boon} boon`;
  return fix.models.length === 1 ? `Turn on ${what} for ${fix.models[0]}` : `Turn on ${what} for ${fix.models.length} models`;
}

/** The confirmation an Apply asks for. */
export function fixConfirm(fix: AdviceFixView): { title: string; description: string } {
  const what = BOON_LABEL[fix.boon] ?? `the ${fix.boon} boon`;
  return {
    title: `${fixLabel(fix)}?`,
    description: `${fix.models.join(", ")}. Each model keeps its other boons and settings. You can turn it off again on the model's page.`,
  };
}

function pct(p: number): string {
  return `${Math.round(p * 100)}%`;
}

/** The judge's view, in one sentence. */
export function judgeLine(j: AdviceJudgmentView): string {
  const parts = [
    `users affected ${pct(j.hurting_users)}`,
    `worth acting on today ${pct(j.worth_acting)}`,
  ];
  if (j.fix_is_safe !== undefined) parts.push(`the fix is safe ${pct(j.fix_is_safe)}`);
  return `${j.model} rates it: ${parts.join(", ")}.`;
}

/** Findings to act on or look at, and the rest. */
export function splitAdvice(report: AdviceReportView | null): { needs: AdviceFindingView[]; notes: AdviceFindingView[] } {
  const findings = report?.findings ?? [];
  return {
    needs: findings.filter((f) => f.severity !== "note"),
    notes: findings.filter((f) => f.severity === "note"),
  };
}

function day(ms: number): string {
  return new Date(ms).toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

/** "Back on Nov 13 · set aside by a@b" or "Dismissed by a@b on Nov 6". */
export function snoozedLine(s: SnoozedAdviceView): string {
  const by = s.by ? ` by ${s.by}` : "";
  return s.until_ms ? `Back on ${day(s.until_ms)} · set aside${by}` : `Dismissed${by} on ${day(s.at_ms)}`;
}

/** Where the report's numbers come from, and who ordered it. */
export function adviceSource(report: AdviceReportView, now = Date.now()): string {
  const mins = Math.max(0, Math.round((now - report.generated_at_ms) / 60_000));
  const when = mins === 0 ? "just now" : `${mins} min ago`;
  const base = `From the last ${report.window_hours} hours of requests, checked ${when}.`;
  if (report.judge.model) {
    const state = report.judge.pending ? "; it is still judging some findings" : report.judge.error ? `; its last pass failed: ${report.judge.error}` : "";
    return `${base} Ordered by ${report.judge.model}${state}.`;
  }
  return report.judge.error ? `${base} Not judged: ${report.judge.error}.` : base;
}

export const SNOOZE_OPTIONS: { label: string; ms?: number }[] = [
  { label: "Set aside for a day", ms: 86_400_000 },
  { label: "Set aside for a week", ms: 7 * 86_400_000 },
  { label: "Dismiss" },
];
