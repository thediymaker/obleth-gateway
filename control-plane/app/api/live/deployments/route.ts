import { liveRoute } from "@/lib/auth/guard";
import { loadDeployments } from "@/lib/deployments-data";

// Every deployment's models, specs, replicas and discovery, for the list to
// refresh from. A deployment's own page reads ./[id]; the 24 h request counts
// are ./requests.
export const GET = liveRoute(() => loadDeployments());
