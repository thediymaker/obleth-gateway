import type { AdminBatch, BatchStatus } from "@/lib/obleth";

/** Statuses that still have work ahead of them. */
const ACTIVE: BatchStatus[] = ["validating", "in_progress", "finalizing", "cancelling"];

export function isActive(status: BatchStatus): boolean {
  return ACTIVE.includes(status);
}

/** Requests that reached an answer, either way. */
export function doneOf(b: Pick<AdminBatch, "completed" | "failed">): number {
  return b.completed + b.failed;
}

/** Share of the batch's requests that have finished, 0 to 100. */
export function percentOf(b: Pick<AdminBatch, "completed" | "failed" | "total">): number {
  if (b.total <= 0) return 0;
  return Math.min(100, Math.floor((doneOf(b) / b.total) * 100));
}

/** A length of time in words: "45 s", "12 min", "3 h 5 min", "2 d 4 h". */
export function duration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

function secsBetween(from: string | null | undefined, now: number): number | null {
  if (!from) return null;
  const t = Date.parse(from);
  return Number.isNaN(t) ? null : (now - t) / 1000;
}

/**
 * Seconds until the batch's remaining requests finish at the pace so far, or
 * null while there is no pace to go by (nothing finished yet, or not running).
 */
export function etaSecs(b: AdminBatch, now: number): number | null {
  if (b.status !== "in_progress") return null;
  const done = doneOf(b);
  const remaining = b.total - done;
  const running = secsBetween(b.in_progress_at ?? b.created_at, now);
  if (done <= 0 || remaining <= 0 || !running || running <= 0) return null;
  return remaining / (done / running);
}

/** The name the submitter gave the batch, if any. */
export function batchName(b: Pick<AdminBatch, "metadata">): string | null {
  const m = b.metadata;
  if (!m) return null;
  const name = m.name ?? m.title ?? m.description;
  return typeof name === "string" && name.trim() ? name.trim() : null;
}

/** Who submitted it: tenant, then key. */
export function ownerOf(b: Pick<AdminBatch, "tenant_name" | "key_name" | "tenant_id">): string {
  return [b.tenant_name ?? b.tenant_id.slice(0, 8), b.key_name].filter(Boolean).join(" · ");
}

function firstError(b: AdminBatch): string | null {
  const e = b.errors;
  const list = Array.isArray(e) ? e : e?.data;
  const first = list?.[0];
  if (!first?.message) return null;
  return first.line != null ? `Line ${first.line}: ${first.message}` : first.message;
}

export type BatchTone = "running" | "waiting" | "done" | "problem" | "stopped";

export interface BatchState {
  label: string;
  tone: BatchTone;
  /** One plain sentence on what is happening, or null. */
  detail: string | null;
}

/** What a batch is doing, in plain words. */
export function batchState(b: AdminBatch, now: number): BatchState {
  const remaining = b.total - doneOf(b);
  switch (b.status) {
    case "validating":
      return { label: "Checking the file", tone: "running", detail: null };
    case "in_progress": {
      if (b.waiting) {
        const idle = secsBetween(b.last_progress_at ?? b.in_progress_at, now);
        return {
          label: "Waiting for capacity",
          tone: "waiting",
          detail: `Nothing has finished in ${idle != null ? duration(idle) : "a while"}. Batches run only while no one is waiting for the model, so it is likely busy.`,
        };
      }
      const eta = etaSecs(b, now);
      return {
        label: "Running",
        tone: "running",
        detail: eta != null ? `About ${duration(eta)} left at this pace` : `${remaining.toLocaleString()} requests to go`,
      };
    }
    case "finalizing":
      return { label: "Writing results", tone: "running", detail: null };
    case "completed":
      return {
        label: "Done",
        tone: b.failed > 0 ? "problem" : "done",
        detail: b.failed > 0 ? `${b.failed.toLocaleString()} request${b.failed === 1 ? "" : "s"} failed` : null,
      };
    case "failed":
      return { label: "Couldn't start", tone: "problem", detail: firstError(b) ?? "The input file had errors" };
    case "expired":
      return {
        label: "Ran out of time",
        tone: "problem",
        detail: `${remaining.toLocaleString()} request${remaining === 1 ? "" : "s"} didn't run before the ${b.completion_window} deadline`,
      };
    case "cancelling":
      return { label: "Cancelling", tone: "stopped", detail: "Requests already running finish first" };
    case "cancelled":
      return { label: "Cancelled", tone: "stopped", detail: null };
    default:
      return { label: b.status, tone: "stopped", detail: null };
  }
}

/** The filters the page offers, and the status query each sends. */
export const BATCH_FILTERS = {
  active: { label: "Active", status: "active" },
  completed: { label: "Done", status: "completed" },
  expired: { label: "Ran out of time", status: "expired" },
  failed: { label: "Couldn't start", status: "failed" },
  cancelled: { label: "Cancelled", status: "cancelled" },
  all: { label: "All", status: undefined },
} as const;

export type BatchFilter = keyof typeof BATCH_FILTERS;

export function isBatchFilter(v: unknown): v is BatchFilter {
  return typeof v === "string" && v in BATCH_FILTERS;
}

/** How many batches each filter holds, from the status counts. */
export function filterCount(filter: BatchFilter, counts: Partial<Record<BatchStatus, number>>): number {
  const n = (s: BatchStatus) => counts[s] ?? 0;
  switch (filter) {
    case "active":
      return ACTIVE.reduce((sum, s) => sum + n(s), 0);
    case "all":
      return Object.values(counts).reduce((sum, v) => sum + (v ?? 0), 0);
    default:
      return n(filter);
  }
}
