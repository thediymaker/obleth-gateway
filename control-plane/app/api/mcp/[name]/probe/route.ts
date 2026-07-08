import { NextRequest } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { probeServer } from "@/lib/charo/mcp/probe";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  req: NextRequest,
  { params }: { params: Promise<{ name: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;

  const { name } = await params;
  const row = await probeServer(name, { signal: req.signal });
  return Response.json(row);
}
