import { revalidateTag } from "next/cache";
import { NextRequest, NextResponse } from "next/server";
import { CACHE_TAGS, obleth } from "@/lib/obleth";
import { guardAdmin, guardAdminSession } from "@/lib/auth/guard";

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
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const body = await req.json();
    const created = await obleth.createCollection(body, { auditActor: session.email });
    revalidateTag(CACHE_TAGS.knowledgeCollections, "max");
    return NextResponse.json(created);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
