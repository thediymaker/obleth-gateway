import { NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { loadOverviewContext } from "@/lib/overview-data";

export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  return NextResponse.json(await loadOverviewContext());
}
