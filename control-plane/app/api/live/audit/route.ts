import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// The audit log, narrowed by the page's filters; `before_id` pages back.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const p = req.nextUrl.searchParams;
  const n = (k: string) => (p.get(k) ? Number(p.get(k)) : undefined);
  try {
    return NextResponse.json(
      await obleth.auditQuery({
        limit: n("limit"),
        actor: p.get("actor") ?? undefined,
        entityType: p.get("entity_type") ?? undefined,
        entityId: p.get("entity_id") ?? undefined,
        action: p.get("action") ?? undefined,
        since: p.get("since") ?? undefined,
        until: p.get("until") ?? undefined,
        beforeId: n("before_id"),
        q: p.get("q") ?? undefined,
      }),
    );
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
