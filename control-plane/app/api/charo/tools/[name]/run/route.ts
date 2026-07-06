import { NextRequest } from "next/server";
import { guardAdmin } from "@/lib/auth/guard";
import { obleth } from "@/lib/obleth";
import { gatewayChat } from "@/lib/charo/gateway";
import { sse } from "@/lib/charo/sse";
import { ensureToolsRegistered } from "@/lib/charo/tools";
import { runTool } from "@/lib/charo/tools/executor";
import { getTool } from "@/lib/charo/tools/registry";
import type { ToolCtx } from "@/lib/charo/tools/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  req: NextRequest,
  { params }: { params: Promise<{ name: string }> },
) {
  const denied = await guardAdmin();
  if (denied) return denied;

  ensureToolsRegistered();
  const { name } = await params;
  if (!getTool(name)) return new Response("unknown tool", { status: 404 });

  let rawArgs: unknown = {};
  try { rawArgs = await req.json(); } catch { /* empty body ok */ }

  const settings = await obleth.getCharoSettings().catch(() => null);
  if (!settings) return new Response("settings unavailable", { status: 503 });

  const stream = new ReadableStream<Uint8Array>({
    async start(controller) {
      const enc = new TextEncoder();
      let gone = false;
      const send = (event: string, data: unknown) => {
        if (gone) return;
        try { controller.enqueue(enc.encode(sse(event, data))); } catch { gone = true; }
      };
      const ctx: ToolCtx = { settings, gatewayChat, signal: req.signal };
      try {
        send("tool_call", { name, args: rawArgs });
        const env = await runTool(name, rawArgs, ctx, (p) => send("tool_progress", p));
        send("tool_result", env);
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
