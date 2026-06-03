import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(req: NextRequest) {
  try {
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 3_600_000);
    const limitParam = req.nextUrl.searchParams.get("limit");
    const limit = limitParam ? Number(limitParam) : 25;
    const usage = await obleth.usageByKey(sinceMs, limit);
    return NextResponse.json(usage);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
