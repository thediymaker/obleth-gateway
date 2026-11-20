import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps GET/POST /api/v1/knowledge/collections/:id/documents.

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.listDocuments(params.id));

export const POST = liveRoute<{ id: string }>(async (req, { params, session }) =>
  obleth.uploadDocument(params.id, await jsonBody(req), { auditActor: session.email }),
);
