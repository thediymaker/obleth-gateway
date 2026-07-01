import { NextResponse } from "next/server";
import { fetchOverviewSummary } from "@/lib/overview-summary";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const summary = await fetchOverviewSummary();
    return NextResponse.json(summary);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
