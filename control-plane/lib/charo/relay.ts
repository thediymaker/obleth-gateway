function firstChoice(chunk: unknown): Record<string, unknown> | null {
  if (!chunk || typeof chunk !== "object") return null;
  const choices = (chunk as { choices?: unknown }).choices;
  if (!Array.isArray(choices) || choices.length === 0) return null;
  return choices[0] as Record<string, unknown>;
}

export function deltaText(chunk: unknown): string {
  const c = firstChoice(chunk);
  const content = (c?.delta as { content?: unknown } | undefined)?.content;
  return typeof content === "string" ? content : "";
}

export function deltaToolCalls(chunk: unknown): unknown {
  const c = firstChoice(chunk);
  return (c?.delta as { tool_calls?: unknown } | undefined)?.tool_calls;
}

export function finishReason(chunk: unknown): string | undefined {
  const c = firstChoice(chunk);
  return typeof c?.finish_reason === "string" ? c.finish_reason : undefined;
}
