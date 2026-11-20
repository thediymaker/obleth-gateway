import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps POST /api/v1/knowledge/collections/:id/search.

export const POST = liveRoute<{ id: string }>(async (req, { params }) => obleth.searchCollection(params.id, await jsonBody(req)));
