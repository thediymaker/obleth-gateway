import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try {
    const view = await obleth.fairshareLive();
    return NextResponse.json(view);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
