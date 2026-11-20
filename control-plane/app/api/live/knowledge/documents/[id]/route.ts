import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Wraps DELETE /api/v1/knowledge/documents/:id.

export const DELETE = liveRoute<{ id: string }>(async (_req, { params, session }) => {
  await obleth.deleteDocument(params.id, { auditActor: session.email });
  return { deleted: true };
});
