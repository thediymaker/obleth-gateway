import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Wraps POST /api/v1/knowledge/collections/:id/reindex.

export async function POST(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.reindexCollection(id));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
