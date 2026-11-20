import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps GET/PUT /api/v1/settings/knowledge.

export const GET = liveRoute(() => obleth.getKnowledgeSettings());

export const PUT = liveRoute(async (req, { session }) =>
  obleth.updateKnowledgeSettings(await jsonBody(req), { auditActor: session.email }),
);
