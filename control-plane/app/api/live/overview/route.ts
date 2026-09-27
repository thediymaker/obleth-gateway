import { NextRequest, NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { loadOverviewWindow } from "@/lib/overview-data";
import { isOverviewRange } from "@/lib/overview-model";

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const range = req.nextUrl.searchParams.get("range") ?? "24h";
  if (!isOverviewRange(range)) return NextResponse.json({ error: "range must be 1h, 24h or 7d" }, { status: 400 });
  return NextResponse.json(await loadOverviewWindow(range));
}
