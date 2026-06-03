import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(req: NextRequest) {
  try {
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 3_600_000);
    const usage = await obleth.usageByModel(sinceMs);
    return NextResponse.json(usage);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
