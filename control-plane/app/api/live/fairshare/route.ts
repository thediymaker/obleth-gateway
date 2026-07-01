import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const view = await obleth.fairshareLive();
    return NextResponse.json(view);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
