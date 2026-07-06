import { NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

// Masked Slurm settings view: carries the provisioner heartbeat + last
// reconcile-tick outcome so replica panels can flag frozen/stale state.
export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    return NextResponse.json(await obleth.getSlurmSettings());
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
