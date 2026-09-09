import { NextRequest, NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { assembleTrace } from "@/lib/charo/trace";
import { obleth } from "@/lib/obleth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Single, best-effort trace lookup for one Charo test request. The chat stream
// sends an immediate trace (usually empty, since telemetry flushes on a ~1s
// ticker); the panel then polls this endpoint until the receipt is available.
// Returns `{ trace: null }` while telemetry is still flushing.
export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ requestId: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const { requestId } = await params;
  const [spans, logs] = await Promise.all([
    obleth.getRequestSpans(requestId).catch(() => []),
    obleth.usageLogs({ requestId, limit: 1 }).catch(() => []),
  ]);
  if (spans.length === 0 && logs.length === 0) {
    return NextResponse.json({ trace: null });
  }
  return NextResponse.json({ trace: assembleTrace(logs[0] ?? null, spans) });
}
