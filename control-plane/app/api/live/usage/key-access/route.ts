import { NextRequest, NextResponse } from "next/server";
import { KEY_REJECTION_ROWS, obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Where keys were used from, and where they were refused, in one window.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const p = req.nextUrl.searchParams;
  const sinceMs = Number(p.get("since_ms")) || undefined;
  const tenantId = p.get("tenant_id") || undefined;
  const keyId = p.get("key_id") || undefined;
  try {
    const [sources, rejections] = await Promise.all([
      obleth.keySources({ sinceMs, tenantId, keyId }),
      obleth.keyRejections({ sinceMs, tenantId, keyId, limit: KEY_REJECTION_ROWS }),
    ]);
    return NextResponse.json({ sinceMs: sinceMs ?? null, sources, rejections });
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
