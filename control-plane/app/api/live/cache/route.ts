import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

const DAY_MS = 86_400_000;

export async function GET(req: NextRequest) {
  try {
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - DAY_MS);
    const stats = await obleth.cacheStats(sinceMs);
    return NextResponse.json(stats);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
