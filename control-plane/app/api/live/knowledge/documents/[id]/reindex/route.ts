import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdminSession } from "@/lib/auth/guard";

// Wraps POST /api/v1/knowledge/documents/:id/reindex.

export async function POST(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.reindexDocument(id, { auditActor: session.email }));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
