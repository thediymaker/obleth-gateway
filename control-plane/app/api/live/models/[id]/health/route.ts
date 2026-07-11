import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Per-model health detail (config + recent checks), fetched lazily when a
// model card is expanded instead of fanned out for every model on page load.
export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.modelHealthDetail(id));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
