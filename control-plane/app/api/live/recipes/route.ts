import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try { return NextResponse.json(await obleth.listRecipes()); }
  catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
export async function POST(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const b = (await req.json()) as { name: string; body: string; author?: string };
    return NextResponse.json(await obleth.createRecipe(b));
  } catch (e) { return NextResponse.json({ error: String(e) }, { status: 502 }); }
}
