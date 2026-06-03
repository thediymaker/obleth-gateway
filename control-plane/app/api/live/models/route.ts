import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try {
    const models = await obleth.listModels();
    return NextResponse.json(models);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
