import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ request_id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const { request_id } = await params;
    const spans = await obleth.getRequestSpans(request_id);
    return NextResponse.json(spans);
  } catch {
    return NextResponse.json({ error: "spans unavailable" }, { status: 502 });
  }
}
