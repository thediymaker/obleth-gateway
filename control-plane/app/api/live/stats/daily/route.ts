import { obleth } from "@/lib/obleth";
import { liveRoute, RouteError } from "@/lib/auth/guard";
import { numParam } from "@/lib/log-params";

// MCP calls per server or knowledge searches per collection, per day.
export const GET = liveRoute((req) => {
  const p = req.nextUrl.searchParams;
  const kind = p.get("kind");
  if (kind !== "mcp" && kind !== "knowledge") throw new RouteError(400, "kind must be mcp or knowledge");
  return obleth.dailyStats(kind, numParam(p, "days", 7));
});
