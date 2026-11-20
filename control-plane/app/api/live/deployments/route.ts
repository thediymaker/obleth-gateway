import { liveRoute } from "@/lib/auth/guard";
import { loadDeployments } from "@/lib/deployments-data";

// Every deployment's models, specs, replicas and discovery, for the list and a
// deployment's page to refresh from.
export const GET = liveRoute(() => loadDeployments());
