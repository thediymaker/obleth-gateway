// Reading an OpenAI-style chat completion, streamed or buffered.

function firstChoice(chunk: unknown): Record<string, unknown> | null {
  if (!chunk || typeof chunk !== "object") return null;
  const choices = (chunk as { choices?: unknown }).choices;
  if (!Array.isArray(choices) || choices.length === 0) return null;
  return choices[0] as Record<string, unknown>;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/**
 * A model's thinking, from a delta or a message. Reasoning parsers put it in
 * its own field: `reasoning` on current vLLM and SGLang, `reasoning_content`
 * on older servers.
 */
function reasoningOf(part: unknown): string {
  const p = part as { reasoning?: unknown; reasoning_content?: unknown } | undefined;
  return text(p?.reasoning ?? p?.reasoning_content);
}

/** The assistant text delta of one streaming chunk. */
export function deltaText(chunk: unknown): string {
  return text((firstChoice(chunk)?.delta as { content?: unknown } | undefined)?.content);
}

/** The reasoning delta of one streaming chunk. */
export function deltaReasoning(chunk: unknown): string {
  return reasoningOf(firstChoice(chunk)?.delta);
}

export function deltaToolCalls(chunk: unknown): unknown {
  const c = firstChoice(chunk);
  return (c?.delta as { tool_calls?: unknown } | undefined)?.tool_calls;
}

export function finishReason(chunk: unknown): string | undefined {
  const c = firstChoice(chunk);
  return typeof c?.finish_reason === "string" ? c.finish_reason : undefined;
}

/** The assistant text of a non-streaming completion. */
export function messageContent(body: unknown): string {
  return text((firstChoice(body)?.message as { content?: unknown } | undefined)?.content);
}

/** The reasoning of a non-streaming completion. */
export function messageReasoning(body: unknown): string {
  return reasoningOf(firstChoice(body)?.message);
}

/**
 * Read an upstream SSE body and call `onData` with each JSON `data:` payload,
 * in order, as it arrives. Events may be LF- or CRLF-framed and split across
 * reads. Comments, other fields and payloads that are not JSON (keep-alives)
 * are skipped. Reading stops at `[DONE]` or the end of the body, and an event
 * left unterminated at the end is still read.
 */
export async function readUpstreamSse(res: Response, onData: (chunk: unknown) => void): Promise<void> {
  if (!res.body) return;
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  // False once `[DONE]` is seen.
  const frame = (raw: string): boolean => {
    for (const line of raw.split("\n")) {
      const t = line.trim();
      if (!t.startsWith("data:")) continue;
      const payload = t.slice(5).trim();
      if (payload === "[DONE]") return false;
      let chunk: unknown;
      try {
        chunk = JSON.parse(payload);
      } catch {
        continue;
      }
      onData(chunk);
    }
    return true;
  };
  try {
    for (;;) {
      const { value, done } = await reader.read();
      buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
      // A CR at the very end may be the first half of a CRLF split across reads.
      buffer = buffer.replace(done ? /\r\n?/g : /\r\n|\r(?!$)/g, "\n");
      let sep: number;
      while ((sep = buffer.indexOf("\n\n")) !== -1) {
        const raw = buffer.slice(0, sep);
        buffer = buffer.slice(sep + 2);
        if (!frame(raw)) return;
      }
      if (done) {
        if (buffer.trim()) frame(buffer);
        return;
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
}
