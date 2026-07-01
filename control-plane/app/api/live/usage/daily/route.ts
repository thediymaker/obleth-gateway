import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import type { UsageDailyGroupBy } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

function defaultStart(): string {
  const d = new Date(Date.now() - 7 * 86_400_000);
  return d.toISOString().slice(0, 10);
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const p = req.nextUrl.searchParams;
    const rows = await obleth.usageDaily({
      startDay: p.get("start_day") ?? defaultStart(),
      endDay: p.get("end_day") ?? today(),
      groupBy: (p.get("group_by") as UsageDailyGroupBy | null) ?? undefined,
      tenantId: p.get("tenant_id") ?? undefined,
      keyId: p.get("key_id") ?? undefined,
      model: p.get("model") ?? undefined,
    });
    return NextResponse.json(rows);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
