import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Masked Slurm settings view: carries the provisioner heartbeat + last
// reconcile-tick outcome so replica panels can flag frozen/stale state.
export const GET = liveRoute(() => obleth.getSlurmSettings());
