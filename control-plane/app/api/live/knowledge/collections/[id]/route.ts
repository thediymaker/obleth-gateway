import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps GET/PUT/DELETE /api/v1/knowledge/collections/:id.

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.getCollection(params.id));

export const PUT = liveRoute<{ id: string }>(async (req, { params, session }) =>
  obleth.updateCollection(params.id, await jsonBody(req), { auditActor: session.email }),
);

export const DELETE = liveRoute<{ id: string }>(async (_req, { params, session }) => {
  await obleth.deleteCollection(params.id, { auditActor: session.email });
  return { deleted: true };
});
