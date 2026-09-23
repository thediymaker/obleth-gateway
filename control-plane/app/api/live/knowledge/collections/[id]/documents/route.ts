import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin, guardAdminSession } from "@/lib/auth/guard";

// Wraps GET/POST /api/v1/knowledge/collections/:id/documents.

export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.listDocuments(id));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}

export async function POST(
  req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    const body = await req.json();
    return NextResponse.json(await obleth.uploadDocument(id, body, { auditActor: session.email }));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
