import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { guardAdmin } from "@/lib/auth/guard";

// Requests and failures over time for the request log, with the log's filters.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const sp = req.nextUrl.searchParams;
  const bucket = Number(sp.get("bucket_ms"));
  try {
    return NextResponse.json(
      await obleth.usageLogHistogram({ ...logParamsFrom(sp), bucketMs: Number.isFinite(bucket) && bucket > 0 ? bucket : undefined }),
    );
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
