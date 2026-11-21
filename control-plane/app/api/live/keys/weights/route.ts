import { liveRoute } from "@/lib/auth/guard";
import { obleth } from "@/lib/obleth";
import { strParam } from "@/lib/log-params";

// One tenant's keys' weights and on/off state, for what a key's weight means
// against its siblings in the key panel.
export const GET = liveRoute(async (req) => {
  const tenantId = strParam(req.nextUrl.searchParams, "tenant_id");
  if (!tenantId) return [];
  return obleth.listKeyFields(["id", "weight", "disabled"] as const, tenantId);
});
