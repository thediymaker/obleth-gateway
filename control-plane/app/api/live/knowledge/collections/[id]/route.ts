import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin, guardAdminSession } from "@/lib/auth/guard";

// Wraps GET/PUT/DELETE /api/v1/knowledge/collections/:id.

export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.getCollection(id));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}

export async function PUT(
  req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    const body = await req.json();
    return NextResponse.json(await obleth.updateCollection(id, body, { auditActor: session.email }));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}

export async function DELETE(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    await obleth.deleteCollection(id, { auditActor: session.email });
    return NextResponse.json({ deleted: true });
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
