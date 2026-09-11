import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Wraps GET/POST /api/v1/knowledge/collections.

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.listCollections());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}

export async function POST(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const body = await req.json();
    return NextResponse.json(await obleth.createCollection(body));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
