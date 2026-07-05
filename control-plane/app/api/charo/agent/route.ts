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
import type { ToolCtx } from "@/lib/charo/tools/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const AGENT_PERSONA =
  "You are Charo, an operator's assistant inside the obleth AI gateway. You can run " +
  "tools on the operator's behalf — currently a capacity benchmark. When the operator " +
  "asks to test or benchmark a model, call run_benchmark with that model's name. Keep a " +
  "dry, unhurried voice; answer the actual question with a point of view, no padding. " +
  "Do not narrate your own plumbing. When a tool returns, summarise the result plainly.";

const MAX_ITERS = 4;

interface AgentBody { messages?: ChatMessage[]; subjectModel?: string }

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

        const schemas = isAdmin && settings ? toolSchemas(settings) : [];
        const transcript: ChatMessage[] = [{ role: "system", content: AGENT_PERSONA }, ...history];
        const ctx: ToolCtx = { settings: settings!, gatewayChat, signal: req.signal };

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
          transcript.push({ role: "assistant", content: assistantText });
          const call = calls[0];
          const tool = getTool(call.name);
          let args: unknown = {};
          try { args = JSON.parse(call.arguments || "{}"); } catch { /* leave {} */ }

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
