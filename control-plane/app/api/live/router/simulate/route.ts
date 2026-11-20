import { obleth, type SimulateRouteRequest } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

export const POST = liveRoute(async (req) => obleth.simulateRoute(await jsonBody<SimulateRouteRequest>(req)));
