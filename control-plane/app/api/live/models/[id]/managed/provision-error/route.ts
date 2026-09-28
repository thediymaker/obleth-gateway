import { NextRequest, NextResponse } from "next/server";
import { obleth } from "@/lib/obleth";
import { guardAdminSession } from "@/lib/auth/guard";

// Clears the provisioner's last submit failure so the dashboard banner can be
// dismissed once the operator has fixed the cause (account/partition/QoS).
export async function PATCH(
  _req: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { denied, session } = await guardAdminSession();
  if (denied) return denied;
  try {
    const { id } = await params;
    return NextResponse.json(await obleth.clearProvisionError(id, { auditActor: session.email }));
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
