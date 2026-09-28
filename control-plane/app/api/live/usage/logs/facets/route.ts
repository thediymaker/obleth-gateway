import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { logParamsFrom } from "@/lib/log-params";
import { guardAdmin } from "@/lib/auth/guard";

// What the request log's matching requests are made of, with the log's filters.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.usageLogFacets(logParamsFrom(req.nextUrl.searchParams)));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
