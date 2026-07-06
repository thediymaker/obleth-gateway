import { getTool } from "./registry";
import type { ToolCtx, ToolProgress, ToolResultEnvelope } from "./types";

export async function runTool(
  name: string,
  rawArgs: unknown,
  ctx: ToolCtx,
  emit: (p: ToolProgress) => void,
): Promise<ToolResultEnvelope> {
  const tool = getTool(name);
  if (!tool) return { type: "tool_error", data: { message: `unknown tool: ${name}` } };
  try {
    const args = tool.parseArgs(rawArgs);
    const data = await tool.run(args, ctx, emit);
    return { type: tool.resultType, data };
  } catch (e) {
    return { type: "tool_error", data: { message: e instanceof Error ? e.message : String(e) } };
  }
}
