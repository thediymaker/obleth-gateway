import { liveRoute } from "@/lib/auth/guard";
import { loadDeploymentRequests } from "@/lib/deployments-data";

// Requests in the last 24 hours by model, for the Deployments list. A
// ClickHouse aggregation, so the list polls it far less often than the fleet.
export const GET = liveRoute(() => loadDeploymentRequests());
