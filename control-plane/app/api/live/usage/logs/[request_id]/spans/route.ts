import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";

export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ request_id: string }> },
) {
  try {
    const { request_id } = await params;
    const spans = await obleth.getRequestSpans(request_id);
    return NextResponse.json(spans);
  } catch {
    return NextResponse.json({ error: "spans unavailable" }, { status: 502 });
  }
}
