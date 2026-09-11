import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Wraps PUT /api/v1/models/:id/knowledge.

export async function PUT(
  req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    const body = (await req.json()) as { collection_ids: string[] };
    return NextResponse.json(
      await obleth.setModelCollections(id, body.collection_ids),
    );
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
