import { NextRequest, NextResponse } from "next/server";
import { obleth, type SavedRecipe } from "@/lib/obleth";

export async function PUT(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  try {
    const { id } = await params;
    const b = (await req.json()) as Omit<SavedRecipe, "id">;
    return NextResponse.json(await obleth.updateRecipe(id, b));
  } catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
export async function DELETE(_req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  try { const { id } = await params; await obleth.deleteRecipe(id); return NextResponse.json({ deleted: true }); }
  catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
