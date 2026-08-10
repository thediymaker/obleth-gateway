// Helpers for chatting with `image`-type models. Those backends serve
// /v1/images/generations, not /v1/chat/completions, so the chat relay turns
// the operator's latest message into a generation prompt and the response's
// image payload into displayable URLs.

import type { ChatMessage } from "./gateway";

/**
 * The prompt for an image generation is the text of the latest user turn —
 * image models have no conversation state, so earlier turns are not relayed.
 */
export function promptFromMessages(messages: ChatMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "user") continue;
    if (typeof m.content === "string") return m.content;
    return m.content
      .map((p) => (p.type === "text" ? p.text : ""))
      .filter(Boolean)
      .join("\n");
  }
  return "";
}

/**
 * Pull displayable image URLs out of a /v1/images/generations response body.
 * Accepts both wire shapes: `b64_json` (requested via `response_format`)
 * becomes a data URL; backends that ignore the hint and return `url` pass
 * through as-is.
 */
export function imageUrls(body: unknown): string[] {
  if (!body || typeof body !== "object") return [];
  const data = (body as { data?: unknown }).data;
  if (!Array.isArray(data)) return [];
  const out: string[] = [];
  for (const item of data) {
    if (!item || typeof item !== "object") continue;
    const { b64_json, url } = item as { b64_json?: unknown; url?: unknown };
    if (typeof b64_json === "string" && b64_json) {
      out.push(`data:image/png;base64,${b64_json}`);
    } else if (typeof url === "string" && url) {
      out.push(url);
    }
  }
  return out;
}
