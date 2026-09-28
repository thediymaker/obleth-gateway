import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdminSession } from "@/lib/auth/guard";

// Wraps DELETE /api/v1/knowledge/documents/:id.

export async function DELETE(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    await obleth.deleteDocument(id, { auditActor: session.email });
    return NextResponse.json({ deleted: true });
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
