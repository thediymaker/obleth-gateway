import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.slurmResources());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
