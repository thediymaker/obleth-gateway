import type { ErrorClass, FaultSide, UsageLogEntry } from "@/lib/obleth";

/**
 * Why a request failed, in plain words, and whose side it was on. The
 * gateway files every failure under one class: the caller's side when the
 * request could not be served as sent, the platform's when a valid request
 * could not be served. Every page that shows a class reads it from here.
 */
export const ERROR_CLASSES: Record<ErrorClass, { label: string; side: FaultSide; hint: string }> = {
  client_input: { label: "Request refused", side: "client", hint: "The request could not be served as sent: a bad field, a prompt longer than the model takes, or images sent to a text-only model." },
  client_cancel: { label: "Caller hung up", side: "client", hint: "The caller went away before the answer was complete." },
  retired: { label: "Retired model", side: "client", hint: "The caller asked for a model that has been retired." },
  limit: { label: "Over limit or budget", side: "client", hint: "A rate limit or budget of the caller's team or key." },
  capacity: { label: "No capacity", side: "platform", hint: "No slot came free in time, or the model had no server ready." },
  upstream_timeout: { label: "Model timed out", side: "platform", hint: "The model server took too long to answer." },
  upstream_error: { label: "Model server error", side: "platform", hint: "The model server failed: an error, a crash partway through the answer, or a stream that broke off." },
  network: { label: "Couldn't reach the model", side: "platform", hint: "The model server could not be reached." },
  gateway: { label: "Gateway error", side: "platform", hint: "The gateway itself failed." },
};

/** Every class, the caller's side first, in the gateway's order. */
export const ERROR_CLASS_IDS = Object.keys(ERROR_CLASSES) as ErrorClass[];

export const SIDE_LABEL: Record<FaultSide, string> = { platform: "Platform", client: "Caller" };

/** Who a side's failures belong to, for sentences: "on the platform's side", "from callers". */
export const SIDE_PHRASE: Record<FaultSide, string> = { platform: "on the platform's side", client: "from callers" };

export function isErrorClass(value: string): value is ErrorClass {
  return Object.hasOwn(ERROR_CLASSES, value);
}

/** A class in plain words; a class this page does not know yet reads as recorded, spaced out. */
export function classLabel(value: string | undefined): string {
  if (!value) return "";
  return isErrorClass(value) ? ERROR_CLASSES[value].label : value.replace(/_/g, " ");
}

/** Whose side a class is on, or null for a success or a class this page does not know. */
export function classSide(value: string | undefined): FaultSide | null {
  return value && isErrorClass(value) ? ERROR_CLASSES[value].side : null;
}

/**
 * Why a failed request failed, as the gateway recorded it. Rows written
 * before reasons were recorded have only their status to go on.
 */
export function failureReason(row: Pick<UsageLogEntry, "status_code" | "error_reason">): string {
  return row.error_reason?.trim() || `HTTP ${row.status_code}`;
}

/**
 * The classes a search names: ids (`capacity`, `upstream-timeout`), several
 * joined by commas. Null when any part names no class.
 */
export function parseClasses(text: string): ErrorClass[] | null {
  const out: ErrorClass[] = [];
  for (const part of text.toLowerCase().split(",").map((p) => p.trim().replace(/-/g, "_")).filter(Boolean)) {
    if (isErrorClass(part)) out.push(part);
    else return null;
  }
  return out.length ? [...new Set(out)] : null;
}

/** `fault:` in the search box: the platform's side, or the callers'. */
export function parseFault(text: string): FaultSide | null {
  const v = text.trim().toLowerCase();
  if (["platform", "ours", "us"].includes(v)) return "platform";
  if (["client", "caller", "callers", "theirs"].includes(v)) return "client";
  return null;
}
