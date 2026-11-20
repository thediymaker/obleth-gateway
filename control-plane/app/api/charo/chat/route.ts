import { NextRequest } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { chatRequestSchema } from "@/lib/charo/chat-request";
import { gatewayChat, gatewayImages, type ChatMessage } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";
import { imageUrls, promptFromMessages } from "@/lib/charo/images";
import { deltaReasoning, deltaText, messageContent, messageReasoning, readUpstreamSse } from "@/lib/charo/relay";
import { sseResponse } from "@/lib/charo/sse";
import { lookupTrace } from "@/lib/charo/trace-lookup";
import { CHARO_PERSONA } from "@/lib/charo/persona";
import { obleth } from "@/lib/obleth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Charo's model-test relay. Streams a multi-turn chat through the gateway's
// NORMAL /v1/chat/completions endpoint (as the reserved internal tenant) so the
// model's configured boons fire exactly as in production, then attaches a
// best-effort, read-only trace "receipt" showing which boons actually fired.
// This is the no-brain / vision fallback path; the persona lives in
// lib/charo/persona.ts so it stays identical to the agent loop.

/**
 * Look up the target model's modality so the relay can pick the matching
 * data-plane endpoint. Fails open to `chat` — an admin-API hiccup should
 * degrade to the old behavior, not block the chat.
 */
async function resolveModelType(name: string): Promise<string> {
  try {
    const models = await obleth.listModels();
    return models.find((m) => m.model_name === name)?.model_type ?? "chat";
  } catch {
    return "chat";
  }
}

export async function POST(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;
  let input: unknown;
  try { input = await req.json(); } catch { return new Response("invalid JSON body", { status: 400 }); }
  const validated = chatRequestSchema.safeParse(input);
  if (!validated.success) return Response.json({ error: validated.error.issues[0]?.message ?? "invalid request" }, { status: 400 });
  const body = validated.data;
  const { model, messages, generation } = body;

  // Prepend the persona unless the caller already supplied a system message or bare mode.
  const hasSystem = messages.some((m) => m.role === "system");
  const outgoing: ChatMessage[] =
    generation?.systemPrompt
      ? [{ role: "system", content: generation.systemPrompt }, ...messages.filter((m) => m.role !== "system")]
      : body.bare || hasSystem ? messages : [{ role: "system", content: CHARO_PERSONA }, ...messages];

  // The endpoint follows the model's modality: image models serve
  // /v1/images/generations, so a chat-completions POST would just 404 upstream.
  const modelKind = await resolveModelType(model);

  return sseResponse(async (send) => {
    const started = performance.now();
    let ttftMs: number | undefined;
    let inputTokens: number | undefined;
    let outputTokens: number | undefined;
    const observe = (payload: unknown) => {
      const value = payload as { usage?: { prompt_tokens?: number; completion_tokens?: number } };
      if (typeof value?.usage?.prompt_tokens === "number") inputTokens = value.usage.prompt_tokens;
      if (typeof value?.usage?.completion_tokens === "number") outputTokens = value.usage.completion_tokens;
    };

    if (modelKind === "image") {
      // One prompt in, one image out — no streaming, no persona.
      const res = await gatewayImages(
        { model, prompt: promptFromMessages(messages), n: 1, response_format: "b64_json" },
        req.signal,
      );
      const requestId = res.headers.get("x-obleth-request-id");
      if (requestId) send("request", { requestId });
      const text = await res.text().catch(() => "");
      let parsed: unknown = null;
      try {
        parsed = JSON.parse(text);
      } catch {
        /* not JSON */
      }
      const images = imageUrls(parsed);
      if (res.ok && images.length > 0) {
        send("image", { images });
      } else {
        send("error", {
          statusCode: res.status,
          message: apiErrorMessage(parsed, text) || res.statusText,
        });
      }
      if (requestId) {
        const trace = await lookupTrace(requestId);
        send("trace", { requestId, trace, statusCode: res.status });
      }
      send("done", {});
      return;
    }
    if (modelKind !== "chat") {
      send("error", {
        message: `${model} is an ${modelKind.replace(/_/g, " ")} model and does not serve chat. Pick a chat or image model.`,
      });
      send("done", {});
      return;
    }

    const res = await gatewayChat(
      { model, messages: outgoing, stream: true, stream_options: { include_usage: true }, ...(generation?.temperature !== undefined ? { temperature: generation.temperature } : {}), ...(generation?.maxTokens !== undefined ? { max_tokens: generation.maxTokens } : {}) },
      req.signal,
    );
    const requestId = res.headers.get("x-obleth-request-id");
    if (requestId) send("request", { requestId });

    const contentType = res.headers.get("content-type") ?? "";
    if (!res.ok || !contentType.includes("text/event-stream")) {
      // Non-streaming response. Usually an error (guardrail block, upstream
      // failure), but the boon fail-open path can return a successful 200
      // buffered completion — surface that as a normal token, not an error.
      const text = await res.text().catch(() => "");
      let parsed: unknown = null;
      try {
        parsed = JSON.parse(text);
      } catch {
        /* not JSON */
      }
      const content = messageContent(parsed);
      const reasoning = messageReasoning(parsed);
      observe(parsed);
      if (res.ok && (content || reasoning)) {
        ttftMs = performance.now() - started;
        if (reasoning) send("reasoning", { text: reasoning });
        if (content) send("token", { text: content });
      } else {
        send("error", {
          statusCode: res.status,
          message: apiErrorMessage(parsed, text) || res.statusText,
        });
      }
    } else {
      // Relay the OpenAI-style SSE token stream. The first token of
      // either kind is the first token: a reasoning model's answer starts
      // after it has thought.
      await readUpstreamSse(res, (chunk) => {
        observe(chunk);
        const reasoning = deltaReasoning(chunk);
        if (reasoning) { ttftMs ??= performance.now() - started; send("reasoning", { text: reasoning }); }
        const text = deltaText(chunk);
        if (text) { ttftMs ??= performance.now() - started; send("token", { text }); }
      });
    }

    send("metrics", { inputTokens, outputTokens, ttftMs, totalMs: performance.now() - started });
    // Best-effort, non-blocking trace receipt.
    if (requestId) {
      const trace = await lookupTrace(requestId);
      send("trace", { requestId, trace, statusCode: res.status });
    }
    send("done", {});
  });
}
