// Server-side gateway client for Charo's model-test console.
//
// Charo tests models by calling the gateway's NORMAL `/v1/chat/completions`
// data-plane endpoint as the reserved, protected internal tenant. Every boon
// configured on the model (web search, MCP/tool loop, vision, guardrails,
// structured output, cache) therefore fires automatically, server-side, exactly
// as it would for any real client — this is a faithful functional test.
//
// The system key secret is fetched once (cached) from the admin API and never
// leaves the server. Only route handlers under `app/api/charo/*` import this.

import { obleth } from "@/lib/obleth";

const PROXY_BASE = process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080";
const KEY_TTL_MS = 5 * 60_000;

let cachedKey: { secret: string; at: number } | null = null;

/** Test-only: clear the in-memory key cache between cases. */
export function __resetKeyCache(): void {
  cachedKey = null;
}

/** Fetch (and briefly cache) the reserved control-plane key secret. */
export async function getControlPlaneKey(): Promise<string> {
  if (cachedKey && Date.now() - cachedKey.at < KEY_TTL_MS) return cachedKey.secret;
  const { secret } = await obleth.controlPlaneKey();
  cachedKey = { secret, at: Date.now() };
  return secret;
}

export type ChatContentPart =
  | { type: "text"; text: string }
  | { type: "image_url"; image_url: { url: string } };

export interface ChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string | ChatContentPart[];
}

export interface GatewayChatBody {
  model: string;
  messages: ChatMessage[];
  stream?: boolean;
  [k: string]: unknown;
}

export interface GatewayImagesBody {
  model: string;
  prompt: string;
  n?: number;
  response_format?: string;
  [k: string]: unknown;
}

/**
 * POST a chat-completions request to the data plane as the reserved tenant.
 * Returns the raw `fetch` Response so callers can read the
 * `x-obleth-request-id` header and stream the body.
 */
export async function gatewayChat(
  body: GatewayChatBody,
  signal?: AbortSignal,
): Promise<Response> {
  return gatewayPost("/v1/chat/completions", body, signal);
}

/**
 * POST an image-generation request to the data plane as the reserved tenant.
 * Used when the chat target is an `image`-type model — those backends serve
 * /v1/images/generations, not /v1/chat/completions.
 */
export async function gatewayImages(
  body: GatewayImagesBody,
  signal?: AbortSignal,
): Promise<Response> {
  return gatewayPost("/v1/images/generations", body, signal);
}

async function gatewayPost(
  path: string,
  body: unknown,
  signal?: AbortSignal,
): Promise<Response> {
  const secret = await getControlPlaneKey();
  return fetch(`${PROXY_BASE}${path}`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${secret}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
    signal,
  });
}
