import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin, guardAdminSession } from "@/lib/auth/guard";

// Wraps GET/PUT /api/v1/settings/knowledge.

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.getKnowledgeSettings());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}

export async function PUT(req: NextRequest) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const body = await req.json();
    return NextResponse.json(await obleth.updateKnowledgeSettings(body, { auditActor: session.email }));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
