"use client";

import { useCallback, useRef, useState } from "react";
import type { CharoState } from "./sprite";
import type { TraceSummary } from "@/lib/charo/trace";
import type { StepOutcome } from "@/lib/charo/bench/types";

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
  toolResults?: { type: string; data: unknown }[];
  /** Live per-step accumulation while a bench tool runs. */
  liveSteps?: StepOutcome[];
  /** Set when a confirmation-gated tool is awaiting the operator. */
  pendingConfirm?: { name: string; args: unknown };
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

/**
 * Read an SSE byte stream, splitting on blank-line frame boundaries and
 * dispatching each `event:`/`data:` frame to `onEvent`. Shared by the legacy
 * chat relay, the agent loop, and direct tool runs so the framing lives once.
 */
async function readSSE(
  res: Response,
  signal: AbortSignal,
  onEvent: (event: string, data: Record<string, unknown>) => void,
): Promise<void> {
  const reader = res.body!.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
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
      if (!data || signal.aborted) continue;
      let parsed: Record<string, unknown> = {};
      try {
        parsed = JSON.parse(data);
      } catch {
        continue;
      }
      onEvent(event, parsed);
    }
  }
}

export function useCharoStream() {
  const [messages, setMessages] = useState<ChatTurn[]>([]);
  const [state, setState] = useState<CharoState>("idle");
  const [busy, setBusy] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  // Background trace polls outlive the stream they belong to; track them so a
  // reset can cancel any still in flight.
  const pollsRef = useRef<Set<AbortController>>(new Set());

  const patchTurn = useCallback(
    (id: string, fn: (t: ChatTurn) => ChatTurn) =>
      setMessages((prev) => prev.map((m) => (m.id === id ? fn(m) : m))),
    [],
  );

  const reset = useCallback(() => {
    abortRef.current?.abort();
    pollsRef.current.forEach((c) => c.abort());
    pollsRef.current.clear();
    setMessages([]);
    setState("idle");
    setBusy(false);
  }, []);

  // Poll the trace endpoint until the receipt flushes (or we give up and clear
  // the pending state). Runs in the background, past the stream's end.
  const pollTrace = useCallback(
    async (assistantId: string, requestId: string) => {
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
              patchTurn(assistantId, (m) => ({ ...m, trace, tracePending: false }));
              return;
            }
          } catch {
            if (pc.signal.aborted) return;
          }
        }
        patchTurn(assistantId, (m) =>
          m.tracePending ? { ...m, tracePending: false, trace: null } : m,
        );
      } finally {
        pollsRef.current.delete(pc);
      }
    },
    [patchTurn],
  );

  // Legacy model-tester relay: stream a single completion from the chosen model
  // through /api/charo/chat (persona prepended server-side), attaching the trace
  // receipt. Used for image (vision) turns and as the no-brain fallback.
  const runLegacyChat = useCallback(
    async (
      model: string,
      wire: WireMessage[],
      assistantId: string,
      signal: AbortSignal,
    ): Promise<boolean> => {
      const res = await fetch("/api/charo/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model, messages: wire }),
        signal,
      });
      if (!res.ok || !res.body) throw new Error(`request failed (${res.status})`);
      let sawError = false;
      await readSSE(res, signal, (event, parsed) => {
        if (event === "token") {
          const t = String(parsed.text ?? "");
          patchTurn(assistantId, (m) => ({ ...m, content: m.content + t }));
        } else if (event === "trace") {
          const trace = (parsed.trace ?? null) as TraceSummary | null;
          patchTurn(assistantId, (m) => ({ ...m, trace, tracePending: trace === null }));
          if (trace === null && typeof parsed.requestId === "string") {
            void pollTrace(assistantId, parsed.requestId);
          }
        } else if (event === "error") {
          sawError = true;
          patchTurn(assistantId, (m) => ({
            ...m,
            error: String(parsed.message ?? "request failed"),
            streaming: false,
          }));
        }
      });
      return sawError;
    },
    [patchTurn, pollTrace],
  );

  // Apply one agent/tool SSE event to the given assistant turn. Shared by the
  // agent loop and the confirmation resume (both speak the same vocabulary).
  const applyAgentEvent = useCallback(
    (
      assistantId: string,
      event: string,
      parsed: Record<string, unknown>,
      onNoBrain: () => void,
      flags: { sawError: boolean; noBrain: boolean },
    ) => {
      if (event === "token") {
        const t = String(parsed.text ?? "");
        patchTurn(assistantId, (m) => ({ ...m, content: m.content + t }));
      } else if (event === "confirm") {
        const name = String(parsed.name ?? "");
        patchTurn(assistantId, (m) => ({
          ...m,
          pendingConfirm: { name, args: parsed.args ?? {} },
          streaming: false,
        }));
      } else if (event === "tool_progress" && parsed.kind === "bench_step" && parsed.step) {
        patchTurn(assistantId, (m) => ({
          ...m,
          liveSteps: [...(m.liveSteps ?? []), parsed.step as StepOutcome],
        }));
      } else if (event === "tool_result") {
        patchTurn(assistantId, (m) => ({
          ...m,
          toolResults: [...(m.toolResults ?? []), parsed as { type: string; data: unknown }],
        }));
      } else if (event === "trace") {
        const trace = (parsed.trace ?? null) as TraceSummary | null;
        patchTurn(assistantId, (m) => ({ ...m, trace, tracePending: trace === null }));
        if (trace === null && typeof parsed.requestId === "string") {
          void pollTrace(assistantId, parsed.requestId);
        }
      } else if (event === "error") {
        // The gateway degrades to legacy mode when no brain is configured; the
        // caller retries via runLegacyChat rather than showing an error.
        if (parsed.message === "no-brain") {
          flags.noBrain = true;
          onNoBrain();
          return;
        }
        flags.sawError = true;
        patchTurn(assistantId, (m) => ({
          ...m,
          error: String(parsed.message ?? "request failed"),
          streaming: false,
        }));
      }
    },
    [patchTurn, pollTrace],
  );

  // Chat entry point. Image turns (vision testing a specific model) and no-brain
  // deployments use the legacy relay; otherwise the message drives Charo's brain
  // through the agent loop, which may pause on a `confirm` for a billed tool.
  const send = useCallback(
    async (model: string, text: string, image?: string) => {
      if (busy || !model || (!text.trim() && !image)) return;

      const userTurn: ChatTurn = { id: uid(), role: "user", content: text, image };
      const assistantId = uid();
      const history = [...messages, userTurn];
      setMessages([
        ...history,
        { id: assistantId, role: "assistant", content: "", streaming: true },
      ]);

      setBusy(true);
      setState("thinking");
      const ac = new AbortController();
      abortRef.current = ac;
      const wire = toWire(history);

      try {
        let sawError = false;
        if (image) {
          // Vision testing goes straight to the model under test.
          sawError = await runLegacyChat(model, wire, assistantId, ac.signal);
        } else {
          const res = await fetch("/api/charo/agent", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ messages: wire, subjectModel: model }),
            signal: ac.signal,
          });
          if (!res.ok || !res.body) throw new Error(`request failed (${res.status})`);
          const flags = { sawError: false, noBrain: false };
          await readSSE(res, ac.signal, (event, parsed) =>
            applyAgentEvent(assistantId, event, parsed, () => {}, flags),
          );
          if (flags.noBrain) {
            // Gateway has no brain configured: fall back to the legacy tester on
            // the selected model, reusing the same assistant turn.
            sawError = await runLegacyChat(model, wire, assistantId, ac.signal);
          } else {
            sawError = flags.sawError;
          }
        }

        patchTurn(assistantId, (m) =>
          m.pendingConfirm ? m : { ...m, streaming: false },
        );
        setState(sawError ? "error" : "result");
      } catch (e) {
        if (!ac.signal.aborted) {
          patchTurn(assistantId, (m) => ({ ...m, error: String(e), streaming: false }));
          setState("error");
        }
      } finally {
        setBusy(false);
        abortRef.current = null;
      }
    },
    [busy, messages, runLegacyChat, applyAgentEvent, patchTurn],
  );

  // The operator approved a confirmation-gated tool. Resume the agent loop: the
  // server runs the tool and the brain delivers a verdict, streamed into a fresh
  // assistant turn. The proposing turn's confirm card is cleared.
  const confirmRun = useCallback(
    async (turnId: string) => {
      if (busy) return;
      const turn = messages.find((m) => m.id === turnId);
      const pending = turn?.pendingConfirm;
      if (!pending) return;
      const wire = toWire(messages);
      // Clear the confirm card on the proposing turn.
      setMessages((prev) =>
        prev.map((m) => (m.id === turnId ? { ...m, pendingConfirm: undefined } : m)),
      );

      const assistantId = uid();
      setMessages((prev) => [
        ...prev,
        { id: assistantId, role: "assistant", content: "", streaming: true, liveSteps: [], toolResults: [] },
      ]);
      setBusy(true);
      setState("thinking");
      const ac = new AbortController();
      abortRef.current = ac;

      try {
        const res = await fetch("/api/charo/agent", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ messages: wire, confirmed: pending }),
          signal: ac.signal,
        });
        if (!res.ok || !res.body) throw new Error(`tool run failed (${res.status})`);
        const flags = { sawError: false, noBrain: false };
        await readSSE(res, ac.signal, (event, parsed) =>
          applyAgentEvent(assistantId, event, parsed, () => {}, flags),
        );
        patchTurn(assistantId, (m) => ({ ...m, streaming: false }));
        setState(flags.sawError ? "error" : "result");
      } catch (e) {
        if (!ac.signal.aborted) {
          patchTurn(assistantId, (m) => ({ ...m, error: String(e), streaming: false }));
          setState("error");
        }
      } finally {
        setBusy(false);
        abortRef.current = null;
      }
    },
    [busy, messages, applyAgentEvent, patchTurn],
  );

  const confirmCancel = useCallback(
    (turnId: string) => {
      patchTurn(turnId, (m) => ({
        ...m,
        pendingConfirm: undefined,
        content: m.content || "(cancelled)",
      }));
    },
    [patchTurn],
  );

  // Run a tool directly (rail button / quick-action), bypassing the brain.
  const runToolDirect = useCallback(
    async (name: string, args: unknown) => {
      if (busy) return;
      const assistantId = uid();
      setMessages((prev) => [
        ...prev,
        { id: assistantId, role: "assistant", content: "", streaming: true, liveSteps: [], toolResults: [] },
      ]);
      setBusy(true);
      setState("thinking");
      const ac = new AbortController();
      abortRef.current = ac;

      try {
        const res = await fetch(`/api/charo/tools/${encodeURIComponent(name)}/run`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(args),
          signal: ac.signal,
        });
        if (!res.ok || !res.body) throw new Error(`tool run failed (${res.status})`);
        let sawError = false;
        await readSSE(res, ac.signal, (event, parsed) => {
          if (event === "tool_progress" && parsed.kind === "bench_step" && parsed.step) {
            patchTurn(assistantId, (m) => ({
              ...m,
              liveSteps: [...(m.liveSteps ?? []), parsed.step as StepOutcome],
            }));
          } else if (event === "tool_result") {
            patchTurn(assistantId, (m) => ({
              ...m,
              toolResults: [...(m.toolResults ?? []), parsed as { type: string; data: unknown }],
            }));
          } else if (event === "error") {
            sawError = true;
            patchTurn(assistantId, (m) => ({
              ...m,
              error: String(parsed.message ?? "tool error"),
              streaming: false,
            }));
          }
        });
        patchTurn(assistantId, (m) => ({ ...m, streaming: false }));
        setState(sawError ? "error" : "result");
      } catch (e) {
        if (!ac.signal.aborted) {
          patchTurn(assistantId, (m) => ({ ...m, error: String(e), streaming: false }));
          setState("error");
        }
      } finally {
        setBusy(false);
        abortRef.current = null;
      }
    },
    [busy, patchTurn],
  );

  return { messages, state, busy, send, reset, runToolDirect, confirmRun, confirmCancel };
}
