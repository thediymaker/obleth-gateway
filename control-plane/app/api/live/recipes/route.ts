import { NextRequest, NextResponse } from "next/server";
import { obleth, type SavedRecipe } from "@/lib/obleth";

export async function GET() {
  try { return NextResponse.json(await obleth.listRecipes()); }
  catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
export async function POST(req: NextRequest) {
  try {
    const b = (await req.json()) as Omit<SavedRecipe, "id">;
    return NextResponse.json(await obleth.createRecipe(b));
  } catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
