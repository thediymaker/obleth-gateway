import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Every capped budget and how much of its current period is used.
export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.budgetUsage());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
