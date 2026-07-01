import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

const DAY_MS = 86_400_000;

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - DAY_MS);
    const stats = await obleth.cacheStats(sinceMs);
    return NextResponse.json(stats);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
