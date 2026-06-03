import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(req: NextRequest) {
  try {
    const bucketMs = Number(req.nextUrl.searchParams.get("bucket_ms") ?? 10_000);
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 1_800_000);
    const series = await obleth.usageSeriesByTenant(bucketMs, sinceMs);
    return NextResponse.json(series);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
