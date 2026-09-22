import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const sinceRaw = req.nextUrl.searchParams.get("since_ms");
    const since_ms = sinceRaw === null || sinceRaw === "" ? undefined : Number(sinceRaw);
    const model = req.nextUrl.searchParams.get("model") ?? undefined;
    const view = await obleth.fairshareHistory({
      since_ms: Number.isFinite(since_ms) ? since_ms : undefined,
      model: model || undefined,
    });
    return NextResponse.json(view);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
