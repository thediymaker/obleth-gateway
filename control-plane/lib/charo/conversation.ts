import type { ChatTurn } from "@/components/charo/use-charo-stream";

export interface Conversation {
  messages: ChatTurn[];
  activeTarget: string | null;
  targetStart: string | null;
}

export function restoreConversation(raw: string | null): Conversation | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw);
    if (!Array.isArray(value.messages) || value.messages.length > 1000 ||
        !value.messages.every((m: ChatTurn) => m && typeof m.id === "string" &&
          typeof m.content === "string" && ["user", "assistant"].includes(m.role))) return null;
    return {
      messages: value.messages.map((m: ChatTurn) => ({ ...m, streaming: false, tracePending: false,
        ...(m.streaming ? { error: "Interrupted when this session closed. Retry to continue." } : {}) })),
      activeTarget: typeof value.activeTarget === "string" ? value.activeTarget : null,
      targetStart: typeof value.targetStart === "string" ? value.targetStart : null,
    };
  } catch { return null; }
}

/** A retry replaces only the last exchange in this lane, retaining its own history. */
export function retryTurn(messages: ChatTurn[]): { turn: ChatTurn; index: number } | null {
  for (let index = messages.length - 1; index >= 0; index--) {
    if (messages[index].role === "user") return { turn: messages[index], index };
  }
  return null;
}
