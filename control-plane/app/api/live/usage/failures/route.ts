import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { guardAdmin } from "@/lib/auth/guard";

// The failures among the requests matching the log's filters, split by whose
// side they were on and grouped into repeats.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.usageFailures(logParamsFrom(req.nextUrl.searchParams)));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
