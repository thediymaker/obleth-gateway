import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Clears the provisioner's last submit failure so the dashboard banner can be
// dismissed once the operator has fixed the cause (account/partition/QoS).
export const PATCH = liveRoute<{ id: string }>((_req, { params, session }) =>
  obleth.clearProvisionError(params.id, { auditActor: session.email }),
);
