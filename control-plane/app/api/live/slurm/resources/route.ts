import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET() {
  try {
    return NextResponse.json(await obleth.slurmResources());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
