import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps GET and PUT /api/v1/models/:id/knowledge.

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.getModelCollections(params.id));

export const PUT = liveRoute<{ id: string }>(async (req, { params, session }) => {
  const body = await jsonBody<{ collection_ids: string[] }>(req);
  return obleth.setModelCollections(params.id, body.collection_ids, { auditActor: session.email });
});
