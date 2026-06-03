import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try {
    const health = await obleth.modelHealth();
    return NextResponse.json(health);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
