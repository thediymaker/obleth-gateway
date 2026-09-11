import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Wraps POST /api/v1/knowledge/collections/:id/search.

export async function POST(
  req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    const body = await req.json();
    return NextResponse.json(await obleth.searchCollection(id, body));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
