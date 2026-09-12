import { NextRequest } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { chatRequestSchema } from "@/lib/charo/chat-request";
import { gatewayChat, gatewayImages, type ChatMessage } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";
import { imageUrls, promptFromMessages } from "@/lib/charo/images";
import { assembleTrace, type TraceSummary } from "@/lib/charo/trace";
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

function sse(event: string, data: unknown): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** Pull the assistant text delta out of one OpenAI-style streaming chunk. */
function deltaText(chunk: unknown): string {
  if (!chunk || typeof chunk !== "object") return "";
  const choices = (chunk as { choices?: unknown }).choices;
  if (!Array.isArray(choices) || choices.length === 0) return "";
  const delta = (choices[0] as { delta?: { content?: unknown } }).delta;
  return typeof delta?.content === "string" ? delta.content : "";
}

/** Pull the assistant text out of a non-streaming chat-completion JSON body. */
function messageContent(body: unknown): string {
  if (!body || typeof body !== "object") return "";
  const choices = (body as { choices?: unknown }).choices;
  if (!Array.isArray(choices) || choices.length === 0) return "";
  const message = (choices[0] as { message?: { content?: unknown } }).message;
  return typeof message?.content === "string" ? message.content : "";
}

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

/**
 * Best-effort, single-shot trace lookup. Telemetry flushes on a ~1s ticker, so
 * this usually comes back empty right after the stream ends — the client polls
 * `/api/charo/trace/:id` to fill it in rather than blocking `done` here.
 */
async function fetchTrace(requestId: string): Promise<TraceSummary | null> {
  const [spans, logs] = await Promise.all([
    obleth.getRequestSpans(requestId).catch(() => []),
    obleth.usageLogs({ requestId, limit: 1 }).catch(() => []),
  ]);
  if (spans.length > 0 || logs.length > 0) {
    return assembleTrace(logs[0] ?? null, spans);
  }
  return null;
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

  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      const enc = new TextEncoder();
      // Once the client disconnects the stream is cancelled and enqueuing
      // throws; swallow sends past that point instead of crashing the handler.
      let clientGone = false;
      const send = (event: string, data: unknown) => {
        if (clientGone) return;
        try {
          controller.enqueue(enc.encode(sse(event, data)));
        } catch {
          clientGone = true;
        }
      };
      const started = performance.now();
      let ttftMs: number | undefined;
      let inputTokens: number | undefined;
      let outputTokens: number | undefined;
      const observe = (payload: unknown) => {
        const value = payload as { usage?: { prompt_tokens?: number; completion_tokens?: number } };
        if (typeof value?.usage?.prompt_tokens === "number") inputTokens = value.usage.prompt_tokens;
        if (typeof value?.usage?.completion_tokens === "number") outputTokens = value.usage.completion_tokens;
      };

      try {
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
            const trace = await fetchTrace(requestId);
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
          observe(parsed);
          if (res.ok && content) {
            ttftMs = performance.now() - started;
            send("token", { text: content });
          } else {
            send("error", {
              statusCode: res.status,
              message: apiErrorMessage(parsed, text) || res.statusText,
            });
          }
        } else {
          // Relay the OpenAI-style SSE token stream.
          const reader = res.body!.getReader();
          const decoder = new TextDecoder();
          let buffer = "";
          let done = false;
          while (!done) {
            const { value, done: streamDone } = await reader.read();
            if (streamDone) break;
            buffer += decoder.decode(value, { stream: true });
            buffer = buffer.replace(/\r\n/g, "\n");
            let sep: number;
            while ((sep = buffer.indexOf("\n\n")) !== -1) {
              const frame = buffer.slice(0, sep);
              buffer = buffer.slice(sep + 2);
              for (const line of frame.split("\n")) {
                const trimmed = line.trim();
                if (!trimmed.startsWith("data:")) continue;
                const payload = trimmed.slice(5).trim();
                if (payload === "[DONE]") {
                  done = true;
                  break;
                }
                try {
                  const parsed = JSON.parse(payload);
                  observe(parsed);
                  const text = deltaText(parsed);
                  if (text) { ttftMs ??= performance.now() - started; send("token", { text }); }
                } catch {
                  /* ignore non-JSON keep-alive frames */
                }
              }
            }
          }
        }

        send("metrics", { inputTokens, outputTokens, ttftMs, totalMs: performance.now() - started });
        // Best-effort, non-blocking trace receipt.
        if (requestId) {
          const trace = await fetchTrace(requestId);
          send("trace", { requestId, trace, statusCode: res.status });
        }
        send("done", {});
      } catch (e) {
        send("error", { message: String(e) });
      } finally {
        if (!clientGone) {
          try {
            controller.close();
          } catch {
            /* already closed/cancelled */
          }
        }
      }
    },
  });

  return new Response(stream, {
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
    },
  });
}
