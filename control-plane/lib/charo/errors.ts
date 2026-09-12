// Shared upstream-error-message extraction for the Charo relays. Both the
// chat relay and the playground's image relay call inference backends
// directly (as the reserved internal tenant) and need to surface whatever
// explanation the backend gave, not a generic fallback string.

/**
 * Pull a human-readable message out of an upstream error body. Covers the
 * OpenAI `{"error": …}` shape and the FastAPI-style `{"detail": …}` many
 * inference servers — including the ComfyUI bridges and sd-webui OpenAI shims
 * the image relay targets — return.
 */
export function apiErrorMessage(parsed: unknown, fallback: string): string {
  const j = parsed as
    | { error?: { message?: string } | string; detail?: unknown }
    | null;
  const fromError = typeof j?.error === "object" ? j?.error?.message : j?.error;
  const fromDetail = typeof j?.detail === "string" ? j.detail : undefined;
  return fromError ?? fromDetail ?? fallback;
}
