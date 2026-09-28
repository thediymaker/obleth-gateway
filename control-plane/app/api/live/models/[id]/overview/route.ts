import { NextRequest, NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { loadModelOverview } from "@/lib/model-overview-data";

// A model page's traffic, tenants and recent changes, in one read.
export async function GET(
  req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const { id } = await params;
  const name = req.nextUrl.searchParams.get("name");
  if (!name) return NextResponse.json({ error: "name is required" }, { status: 400 });
  return NextResponse.json(await loadModelOverview(id, name));
}
