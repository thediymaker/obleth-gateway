import { liveRoute, RouteError } from "@/lib/auth/guard";
import { gatewayMeshGet } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// One mesh job's status and progress, polled by the playground while it runs.
export const GET = liveRoute<{ id: string }>(async (req, { params }) => {
  const res = await gatewayMeshGet(params.id, false, req.signal);
  const json = await res.json().catch(() => null);
  if (!res.ok) throw new RouteError(res.status, apiErrorMessage(json, "job lookup failed"));
  return { job: json };
});
