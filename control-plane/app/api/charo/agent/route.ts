import { NextRequest } from "next/server";
import { getSession } from "@/lib/auth/session";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";
import { gatewayChat, type ChatMessage } from "@/lib/charo/gateway";
import { sse } from "@/lib/charo/sse";
import { ensureToolsRegistered } from "@/lib/charo/tools";
import { toolSchemas, getTool } from "@/lib/charo/tools/registry";
import { runTool } from "@/lib/charo/tools/executor";
import { ToolCallAccumulator } from "@/lib/charo/tools/tool-call-accumulator";
import { deltaText, deltaToolCalls, finishReason } from "@/lib/charo/relay";
import { AGENT_PERSONA } from "@/lib/charo/persona";
import { stripHiddenReasoning } from "@/lib/charo/visible-text";
import type { ToolCtx } from "@/lib/charo/tools/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const MAX_ITERS = 4;

// A schema-only capability (NOT a CharoTool): calling it surfaces an activity's
// guided workflow in the UI. Intercepted below — never executed server-side.
const OPEN_ACTIVITY_SCHEMA = {
  type: "function" as const,
  function: {
    name: "open_activity",
    description:
      "Open a guided activity workflow for the operator, where they pick the model and options in the UI. " +
      "Call this when the operator wants to test a model's capabilities, chat with a specific model, or benchmark one — " +
      "or asks what you can do. Pass the activity id, or omit id to show them the activity menu.",
    parameters: {
      type: "object",
      properties: { id: { type: "string", enum: ["test_capabilities", "chat_with_model", "benchmark"] } },
      additionalProperties: false,
    },
  },
};

interface AgentBody {
  messages?: ChatMessage[];
  subjectModel?: string;
  /** Operator-approved resume of a confirmation-gated tool (see the confirm flow). */
  confirmed?: { name: string; args: unknown };
}

export async function POST(req: NextRequest) {
  const session = await getSession();
  if (!session) return new Response("unauthorized", { status: 401 });

  ensureToolsRegistered();

  let body: AgentBody;
  try { body = (await req.json()) as AgentBody; } catch { return new Response("bad body", { status: 400 }); }
  const history = Array.isArray(body.messages) ? body.messages : [];

  const settings = await obleth.getCharoSettings().catch(() => null);
  const brain = settings?.brain_model?.trim();

  // Tools only for admins; a non-admin gets a plain (toolless) brain chat.
  let isAdmin = false;
  try { await requireAdmin(); isAdmin = true; } catch { isAdmin = false; }

  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      const enc = new TextEncoder();
      let gone = false;
      const send = (event: string, data: unknown) => {
        if (gone) return;
        try { controller.enqueue(enc.encode(sse(event, data))); } catch { gone = true; }
      };

      try {
        // Fail-open: no brain configured → behave like the legacy tester.
        if (!brain) {
          send("error", { message: "no-brain", legacy: true });
          send("done", {});
          return;
        }

        let schemas = isAdmin && settings ? toolSchemas(settings) : [];
        schemas = [...schemas, OPEN_ACTIVITY_SCHEMA];
        const transcript: ChatMessage[] = [{ role: "system", content: AGENT_PERSONA }, ...history];
        const ctx: ToolCtx = { settings: settings!, gatewayChat, signal: req.signal };

        // Confirmation resume: the operator approved a confirmation-gated tool in
        // the UI. Run it server-side, feed the result into the transcript, and drop
        // the tool schemas so the follow-up loop only produces a plain verdict
        // (no re-run). This is the spec's "resume via a short-lived confirmation".
        const confirmed = body.confirmed;
        if (confirmed && isAdmin && getTool(confirmed.name)) {
          send("tool_call", { name: confirmed.name, args: confirmed.args });
          const env = await runTool(confirmed.name, confirmed.args, ctx, (p) =>
            send("tool_progress", p),
          );
          send("tool_result", env);
          transcript.push({
            role: "user",
            content:
              `The ${confirmed.name} tool finished. Result JSON: ` +
              `${JSON.stringify(env.data).slice(0, 2000)}. ` +
              `Give the operator a brief plain verdict; do not call any tool.`,
          });
          schemas = [];
        }

        for (let iter = 0; iter < MAX_ITERS; iter++) {
          const res = await gatewayChat(
            { model: brain, messages: transcript, stream: true, ...(schemas.length ? { tools: schemas } : {}) },
            req.signal,
          );
          if (!res.ok || !res.body) {
            const text = await res.text().catch(() => "");
            send("error", { message: text || `brain error (${res.status})` });
            break;
          }

          const acc = new ToolCallAccumulator();
          let assistantText = "";
          let finish: string | undefined;

          const reader = res.body.getReader();
          const decoder = new TextDecoder();
          let buffer = "";
          let streamDone = false;
          while (!streamDone) {
            const { value, done } = await reader.read();
            if (done) break;
            buffer += decoder.decode(value, { stream: true });
            let sep: number;
            while ((sep = buffer.indexOf("\n\n")) !== -1) {
              const frame = buffer.slice(0, sep); buffer = buffer.slice(sep + 2);
              for (const line of frame.split("\n")) {
                const t = line.trim();
                if (!t.startsWith("data:")) continue;
                const payload = t.slice(5).trim();
                if (payload === "[DONE]") { streamDone = true; break; }
                let chunk: unknown; try { chunk = JSON.parse(payload); } catch { continue; }
                const text = deltaText(chunk);
                if (text) { assistantText += text; send("token", { text }); }
                acc.addDelta(deltaToolCalls(chunk));
                finish = finishReason(chunk) ?? finish;
              }
            }
          }

          const calls = acc.complete().filter((c) => c.name);
          if (finish !== "tool_calls" || calls.length === 0) {
            // Plain answer; done.
            break;
          }

          // Record the assistant's tool-call turn, then handle the FIRST call
          // (one tool per turn — spec non-goal: no intra-turn parallelism).
          transcript.push({ role: "assistant", content: stripHiddenReasoning(assistantText) });
          const call = calls[0];
          let args: unknown = {};
          try { args = JSON.parse(call.arguments || "{}"); } catch { /* leave {} */ }

          if (call.name === "open_activity") {
            const rawId = (args as { id?: unknown }).id;
            const id = typeof rawId === "string" ? rawId : null;
            send("activity", { id });
            break;
          }

          const tool = getTool(call.name);
          if (!tool || !isAdmin) {
            send("tool_result", { type: "tool_error", data: { message: `tool unavailable: ${call.name}` } });
            transcript.push({ role: "tool", content: `tool ${call.name} unavailable` });
            continue;
          }

          if (tool.requiresConfirmation) {
            // Do not execute; hand off to the operator. The client runs it via the
            // deterministic /run route and feeds the summary back on the next turn.
            send("confirm", { name: call.name, args });
            break;
          }

          send("tool_call", { name: call.name, args });
          const env = await runTool(call.name, args, ctx, (p) => send("tool_progress", p));
          send("tool_result", env);
          transcript.push({ role: "tool", content: JSON.stringify(env.data).slice(0, 4000) });
          // loop back for the brain to react
        }

        send("done", {});
      } catch (e) {
        send("error", { message: String(e) });
      } finally {
        if (!gone) { try { controller.close(); } catch { /* closed */ } }
      }
    },
  });

  return new Response(stream, {
    headers: { "Content-Type": "text/event-stream", "Cache-Control": "no-cache, no-transform", Connection: "keep-alive" },
  });
}
