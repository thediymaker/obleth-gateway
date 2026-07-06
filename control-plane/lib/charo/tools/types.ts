import type { CharoSettingsView } from "@/lib/obleth";
import type { gatewayChat } from "@/lib/charo/gateway";

export interface ToolProgress { kind: string; [k: string]: unknown; }
export interface ToolResultEnvelope { type: string; data: unknown; }
export interface ToolCtx {
  settings: CharoSettingsView;
  gatewayChat: typeof gatewayChat;
  signal: AbortSignal;
}
export interface CharoTool<Args = unknown, Result = unknown> {
  name: string;
  description: string;
  parameters: Record<string, unknown>;   // OpenAI JSON-schema function params
  resultType: string;                     // renderer key, e.g. "bench_result"
  requiresConfirmation: boolean;
  parseArgs(raw: unknown): Args;          // validate + apply defaults; throws on bad input
  run(args: Args, ctx: ToolCtx, emit: (p: ToolProgress) => void): Promise<Result>;
}
