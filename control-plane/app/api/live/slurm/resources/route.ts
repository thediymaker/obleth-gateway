import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

export const GET = liveRoute(() => obleth.slurmResources());
