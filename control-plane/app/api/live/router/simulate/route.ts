import { NextResponse } from "next/server";
import { obleth, type SimulateRouteRequest } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function POST(request: Request) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const body = (await request.json()) as SimulateRouteRequest;
    return NextResponse.json(await obleth.simulateRoute(body));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
