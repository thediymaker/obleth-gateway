import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(req: NextRequest) {
  try {
    const model = req.nextUrl.searchParams.get("model") ?? "";
    if (!model) {
      return NextResponse.json({ error: "model is required" }, { status: 400 });
    }
    const bucketMs = Number(req.nextUrl.searchParams.get("bucket_ms") ?? 60_000);
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 3_600_000);
    const series = await obleth.usageSeriesByModel(model, bucketMs, sinceMs);
    return NextResponse.json(series);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
