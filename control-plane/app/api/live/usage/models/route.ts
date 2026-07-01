import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 3_600_000);
    const usage = await obleth.usageByModel(sinceMs);
    return NextResponse.json(usage);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
