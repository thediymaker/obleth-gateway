import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Batches across every tenant for the Batches page's polling; `status`,
// `tenant_id`, `model` and `after` narrow and page the list.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const p = req.nextUrl.searchParams;
  try {
    return NextResponse.json(
      await obleth.listBatches({
        status: p.get("status") ?? undefined,
        tenantId: p.get("tenant_id") ?? undefined,
        model: p.get("model") ?? undefined,
        after: p.get("after") ?? undefined,
        limit: p.get("limit") ? Number(p.get("limit")) : undefined,
      }),
    );
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
