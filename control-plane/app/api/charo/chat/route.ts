import { NextRequest } from "next/server";
import { getSession } from "@/lib/auth/session";
import { gatewayChat, type ChatMessage } from "@/lib/charo/gateway";
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

interface ChatRequestBody {
  model?: string;
  messages?: ChatMessage[];
  /** When true, relay the model raw — do not prepend Charo's persona. */
  bare?: boolean;
}

export async function POST(req: NextRequest) {
  if (!(await getSession())) {
    return new Response("unauthorized", { status: 401 });
  }

  let body: ChatRequestBody;
  try {
    body = (await req.json()) as ChatRequestBody;
  } catch {
    return new Response("invalid JSON body", { status: 400 });
  }

  const model = body.model?.trim();
  const messages = body.messages;
  if (!model || !Array.isArray(messages) || messages.length === 0) {
    return new Response("model and messages are required", { status: 400 });
  }

  // Prepend the persona unless the caller already supplied a system message or bare mode.
  const hasSystem = messages.some((m) => m.role === "system");
  const outgoing: ChatMessage[] =
    body.bare || hasSystem ? messages : [{ role: "system", content: CHARO_PERSONA }, ...messages];

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

      try {
        const res = await gatewayChat(
          { model, messages: outgoing, stream: true },
          req.signal,
        );
        const requestId = res.headers.get("x-obleth-request-id");

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
          if (res.ok && content) {
            send("token", { text: content });
          } else {
            const j = parsed as { error?: { message?: string } | string } | null;
            const message =
              (typeof j?.error === "object" ? j?.error?.message : j?.error) ?? text;
            send("error", { statusCode: res.status, message: message || res.statusText });
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
                  const text = deltaText(JSON.parse(payload));
                  if (text) send("token", { text });
                } catch {
                  /* ignore non-JSON keep-alive frames */
                }
              }
            }
          }
        }

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
