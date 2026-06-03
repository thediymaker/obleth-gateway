import { NextResponse } from "next/server";
import { fetchOverviewSummary } from "@/lib/overview-summary";

export async function GET() {
  try {
    const summary = await fetchOverviewSummary();
    return NextResponse.json(summary);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
