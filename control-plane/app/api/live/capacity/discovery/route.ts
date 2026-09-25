import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// The answering gateway replica's capacity discovery state, polled by the
// model page's capacity tab.
export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.capacityDiscovery());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
