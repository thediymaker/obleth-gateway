import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdminSession } from "@/lib/auth/guard";

export async function PUT(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    const b = (await req.json()) as { name: string; body: string; author?: string };
    return NextResponse.json(await obleth.updateRecipe(id, b, { auditActor: session.email }));
  } catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
export async function DELETE(_req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try { const { id } = await params; await obleth.deleteRecipe(id, { auditActor: session.email }); return NextResponse.json({ deleted: true }); }
  catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
