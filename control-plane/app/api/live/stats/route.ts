import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try {
    const stats = await obleth.stats();
    return NextResponse.json(stats);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
