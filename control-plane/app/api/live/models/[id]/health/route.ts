import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Per-model health detail (config + recent checks), fetched lazily when a
// model card is expanded instead of fanned out for every model on page load.
export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.modelHealthDetail(params.id));
