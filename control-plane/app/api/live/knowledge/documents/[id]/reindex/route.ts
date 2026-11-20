import { obleth } from "@/lib/obleth";
import { liveRoute } from "@/lib/auth/guard";

// Wraps POST /api/v1/knowledge/documents/:id/reindex.

export const POST = liveRoute<{ id: string }>((_req, { params, session }) =>
  obleth.reindexDocument(params.id, { auditActor: session.email }),
);
