import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try { return NextResponse.json(await obleth.listRecipes()); }
  catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
export async function POST(req: NextRequest) {
  try {
    const b = (await req.json()) as { name: string; body: string; author?: string };
    return NextResponse.json(await obleth.createRecipe(b));
  } catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
