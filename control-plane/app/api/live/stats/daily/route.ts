import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// MCP calls per server or knowledge searches per collection, per day.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const kind = req.nextUrl.searchParams.get("kind");
  if (kind !== "mcp" && kind !== "knowledge") return NextResponse.json({ error: "kind must be mcp or knowledge" }, { status: 400 });
  const days = Number(req.nextUrl.searchParams.get("days") ?? 7);
  try {
    return NextResponse.json(await obleth.dailyStats(kind, days));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
