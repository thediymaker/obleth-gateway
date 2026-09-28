import { NextResponse, type NextRequest } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// The Services the kubernetes capacity source can see (cached briefly by the
// gateway), and the model's default Service when `?model=` names one. Read by
// the Service picker on the model page's capacity tab.
export async function GET(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  const model = req.nextUrl.searchParams.get("model") ?? undefined;
  try {
    return NextResponse.json(await obleth.capacityServices(model));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
