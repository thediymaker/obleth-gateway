import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

// Wraps GET/POST /api/v1/knowledge/collections.

export const GET = liveRoute(() => obleth.listCollections());

export const POST = liveRoute(async (req, { session }) => obleth.createCollection(await jsonBody(req), { auditActor: session.email }));
