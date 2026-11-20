import { liveRoute } from "@/lib/auth/guard";
import { loadDeploymentById } from "@/lib/deployments-data";

// One deployment's model, launch settings and replicas, read through the
// per-model endpoints, for its page's poll.
export const GET = liveRoute<{ id: string }>((_req, { params }) => loadDeploymentById(params.id));
