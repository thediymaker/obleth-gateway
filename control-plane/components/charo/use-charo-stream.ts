"use client";

import { useCallback, useRef, useState } from "react";
import type { CharoState } from "./sprite";
import type { TraceSummary } from "@/lib/charo/trace";

export interface ChatTurn {
  id: string;
  role: "user" | "assistant";
  content: string;
  /** Optional image data URL attached to a user turn (vision testing). */
  image?: string;
  trace?: TraceSummary | null;
  /** Set when the trace was requested but hadn't flushed yet. */
  tracePending?: boolean;
  error?: string;
  /** True while the assistant turn is still streaming. */
  streaming?: boolean;
}

type WireContent =
  | string
  | Array<
      | { type: "text"; text: string }
      | { type: "image_url"; image_url: { url: string } }
    >;

interface WireMessage {
  role: "user" | "assistant";
  content: WireContent;
}

function uid(): string {
  return Math.random().toString(36).slice(2);
}

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

// Telemetry flushes to the ledger on a ~1s ticker, so the trace receipt isn't
// queryable the instant the stream ends. Poll for it for a generous window
// before giving up (most requests resolve within the first couple of tries).
const TRACE_POLL_MS = 1200;
const TRACE_POLL_TRIES = 12;

function toWire(turns: ChatTurn[]): WireMessage[] {
  return turns
    .filter((t) => !t.error)
    .map((t) => {
      if (t.role === "user" && t.image) {
        return {
          role: "user" as const,
          content: [
            { type: "text" as const, text: t.content },
            { type: "image_url" as const, image_url: { url: t.image } },
          ],
        };
      }
      return { role: t.role, content: t.content };
    });
}

export function useCharoStream() {
  const [messages, setMessages] = useState<ChatTurn[]>([]);
  const [state, setState] = useState<CharoState>("idle");
  const [busy, setBusy] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  // Background trace polls outlive the stream they belong to; track them so a
  // reset can cancel any still in flight.
  const pollsRef = useRef<Set<AbortController>>(new Set());

  const reset = useCallback(() => {
    abortRef.current?.abort();
    pollsRef.current.forEach((c) => c.abort());
    pollsRef.current.clear();
    setMessages([]);
    setState("idle");
    setBusy(false);
  }, []);

  const send = useCallback(
    async (model: string, text: string, image?: string) => {
      if (busy || !model || (!text.trim() && !image)) return;

      const userTurn: ChatTurn = { id: uid(), role: "user", content: text, image };
      const assistantId = uid();
      // Build the outgoing history explicitly from current state, then set it —
      // don't read it back out of a setState updater (that relies on the updater
      // running synchronously, which React does not guarantee).
      const history = [...messages, userTurn];
      setMessages([
        ...history,
        { id: assistantId, role: "assistant", content: "", streaming: true },
      ]);

      setBusy(true);
      setState("thinking");
      const ac = new AbortController();
      abortRef.current = ac;

      const patch = (fn: (t: ChatTurn) => ChatTurn) =>
        setMessages((prev) => prev.map((m) => (m.id === assistantId ? fn(m) : m)));

      // Poll the trace endpoint until the receipt flushes (or we give up and
      // clear the pending state). Runs in the background, past the stream's end.
      const pollTrace = async (requestId: string) => {
        const pc = new AbortController();
        pollsRef.current.add(pc);
        try {
          for (let i = 0; i < TRACE_POLL_TRIES; i++) {
            await delay(TRACE_POLL_MS);
            if (pc.signal.aborted) return;
            try {
              const r = await fetch(
                `/api/charo/trace/${encodeURIComponent(requestId)}`,
                { signal: pc.signal },
              );
              if (!r.ok) continue;
              const { trace } = (await r.json()) as { trace: TraceSummary | null };
              if (trace) {
                patch((m) => ({ ...m, trace, tracePending: false }));
                return;
              }
            } catch {
              if (pc.signal.aborted) return;
            }
          }
          // Gave up: drop the spinner so the card doesn't hang on "pending".
          patch((m) =>
            m.tracePending ? { ...m, tracePending: false, trace: null } : m,
          );
        } finally {
          pollsRef.current.delete(pc);
        }
      };

      try {
        const res = await fetch("/api/charo/chat", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model, messages: toWire(history) }),
          signal: ac.signal,
        });
        if (!res.ok || !res.body) {
          throw new Error(`request failed (${res.status})`);
        }

        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let sawError = false;

        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          let sep: number;
          while ((sep = buffer.indexOf("\n\n")) !== -1) {
            const frame = buffer.slice(0, sep);
            buffer = buffer.slice(sep + 2);
            let event = "message";
            let data = "";
            for (const line of frame.split("\n")) {
              if (line.startsWith("event:")) event = line.slice(6).trim();
              else if (line.startsWith("data:")) data += line.slice(5).trim();
            }
            if (!data) continue;
            let parsed: Record<string, unknown> = {};
            try {
              parsed = JSON.parse(data);
            } catch {
              continue;
            }

            if (event === "token") {
              const t = String(parsed.text ?? "");
              patch((m) => ({ ...m, content: m.content + t }));
            } else if (event === "trace") {
              const trace = (parsed.trace ?? null) as TraceSummary | null;
              patch((m) => ({ ...m, trace, tracePending: trace === null }));
              // Not flushed yet — keep polling in the background until it is.
              if (trace === null && typeof parsed.requestId === "string") {
                void pollTrace(parsed.requestId);
              }
            } else if (event === "error") {
              sawError = true;
              patch((m) => ({
                ...m,
                error: String(parsed.message ?? "request failed"),
                streaming: false,
              }));
            }
          }
        }

        patch((m) => ({ ...m, streaming: false }));
        setState(sawError ? "error" : "result");
      } catch (e) {
        if (!ac.signal.aborted) {
          patch((m) => ({ ...m, error: String(e), streaming: false }));
          setState("error");
        }
      } finally {
        setBusy(false);
        abortRef.current = null;
      }
    },
    [busy, messages],
  );

  return { messages, state, busy, send, reset };
}
