import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(req: NextRequest) {
  try {
    const model = req.nextUrl.searchParams.get("model") ?? "";
    if (!model) {
      return NextResponse.json({ error: "model is required" }, { status: 400 });
    }
    const sinceMs = Number(req.nextUrl.searchParams.get("since_ms") ?? Date.now() - 3_600_000);
    const limitParam = req.nextUrl.searchParams.get("limit");
    const limit = limitParam == null ? undefined : Number(limitParam);
    const rows = await obleth.usageBreakdownByModel(model, sinceMs, limit);
    return NextResponse.json(rows);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
