import { liveRoute, RouteError } from "@/lib/auth/guard";
import { loadOverviewWindow } from "@/lib/overview-data";
import { isOverviewRange } from "@/lib/overview-model";

export const GET = liveRoute((req) => {
  const range = req.nextUrl.searchParams.get("range") ?? "24h";
  if (!isOverviewRange(range)) throw new RouteError(400, "range must be 1h, 24h or 7d");
  return loadOverviewWindow(range);
});
