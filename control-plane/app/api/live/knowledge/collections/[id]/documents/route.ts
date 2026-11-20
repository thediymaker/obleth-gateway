import { obleth } from "@/lib/obleth";
import { liveRoute, RouteError } from "@/lib/auth/guard";

// Wraps GET/POST /api/v1/knowledge/collections/:id/documents.

/** The gateway's own body limit for an upload: a 50 MiB file base64-encoded,
 *  plus 1 MiB for the JSON around it (`UPLOAD_BODY_LIMIT_BYTES`). */
const MAX_UPLOAD_BODY_BYTES = Math.ceil((50 * 1024 * 1024) / 3) * 4 + 1024 * 1024;

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.listDocuments(params.id));

// The body streams on to the gateway as it arrives, never parsed or copied
// here, so two large uploads at once don't hold four copies each in the
// dashboard pod. The gateway checks the JSON, the size against the knowledge
// settings, and that the file is text; a declared length over its limit is
// refused here before anything is sent.
export const POST = liveRoute<{ id: string }>(async (req, { params, session }) => {
  const declared = Number(req.headers.get("content-length"));
  if (declared > MAX_UPLOAD_BODY_BYTES) {
    throw new RouteError(413, `The upload is ${Math.round(declared / 1024 / 1024)} MB, over the ${Math.round(MAX_UPLOAD_BODY_BYTES / 1024 / 1024)} MB a document can be.`);
  }
  if (!req.body) throw new RouteError(400, "The upload has no body.");
  const res = await obleth.uploadDocument(params.id, req.body, req.headers.get("content-type") ?? "application/json", { auditActor: session.email, signal: req.signal });
  return new Response(res.body, { status: res.status, headers: { "Content-Type": res.headers.get("content-type") ?? "application/json" } });
});
