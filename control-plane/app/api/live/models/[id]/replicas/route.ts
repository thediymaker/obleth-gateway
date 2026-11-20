import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.listReplicas(params.id));
