import { liveRoute, RouteError } from "@/lib/auth/guard";
import { strParam } from "@/lib/log-params";
import { loadModelOverview } from "@/lib/model-overview-data";

// A model page's traffic, tenants and recent changes, in one read.
export const GET = liveRoute<{ id: string }>((req, { params }) => {
  const name = strParam(req.nextUrl.searchParams, "name");
  if (!name) throw new RouteError(400, "name is required");
  return loadModelOverview(params.id, name);
});
