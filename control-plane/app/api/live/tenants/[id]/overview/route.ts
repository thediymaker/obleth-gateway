import { NextRequest, NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { loadTenantOverview } from "@/lib/tenant-overview-data";

// A tenant page's traffic, spend, models, keys and recent changes, in one read.
export async function GET(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const { id } = await params;
  return NextResponse.json(await loadTenantOverview(id));
}
