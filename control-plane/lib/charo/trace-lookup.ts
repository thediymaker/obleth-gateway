import { assembleTrace, type TraceSummary } from "@/lib/charo/trace";
import { obleth } from "@/lib/obleth";

/**
 * One request's trace receipt, or null while its telemetry has not landed
 * yet (it flushes on a ~1 s ticker). Best effort: a failed read is "not yet".
 */
export async function lookupTrace(requestId: string): Promise<TraceSummary | null> {
  const [spans, logs] = await Promise.all([
    obleth.getRequestSpans(requestId).catch(() => []),
    obleth.usageLogs({ requestId, limit: 1 }).catch(() => []),
  ]);
  return spans.length > 0 || logs.length > 0 ? assembleTrace(logs[0] ?? null, spans) : null;
}
