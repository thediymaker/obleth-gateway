import { NextResponse } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { loadDeployments } from "@/lib/deployments-data";

// Every deployment's models, specs, replicas and discovery, for the list and a
// deployment's page to refresh from.
export async function GET() {
  const denied = await guardAdmin();
  if (denied) return denied;
  return NextResponse.json(await loadDeployments());
}
