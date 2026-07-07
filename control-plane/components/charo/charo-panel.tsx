"use client";

import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Send, Square, X, RotateCcw, Maximize2, Minimize2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { TraceCard } from "./trace-card";
import { resultRenderer } from "./results/registry";
import { BenchResultCard } from "./results/bench-result-card";
import { CapabilityResultCard } from "./results/capability-result-card";
import { ConfirmCard } from "./results/confirm-card";
import { WorkflowCard } from "./workflow-card";
import { ActivityCards, ActivityMenu } from "./activity-launcher";
import { ensureActivitiesRegistered, getActivity } from "@/lib/charo/activities";
import type { useCharoStream } from "./use-charo-stream";
import type { CharoState } from "./sprite";

type Stream = ReturnType<typeof useCharoStream>;

const MASCOT: Record<CharoState, string> = {
  idle: "/charo/charo-dark-idle.png",
  held: "/charo/charo-dark-idle.png",
  thinking: "/charo/charo-dark-thinking.png",
  result: "/charo/charo-dark-result.png",
  error: "/charo/charo-dark-error.png",
};

// Animated aura + rotating border, shared by the docked and expanded frames.
const PANEL_CSS = `
  @keyframes charo-border-glow {
    to { --charo-border-angle: 360deg; }
  }
  @keyframes charo-panel-aura {
    0%, 100% { background-position: 0% 50%, 100% 50%; opacity: .2; }
    50% { background-position: 100% 50%, 0% 50%; opacity: .32; }
  }
  @property --charo-border-angle {
    syntax: "<angle>";
    initial-value: 0deg;
    inherits: false;
  }
  .charo-panel-shell::before {
    content: "";
    position: absolute;
    inset: 0;
    pointer-events: none;
    background:
      radial-gradient(circle at 82% 0%, hsl(267 86% 70% / .14), transparent 42%),
      linear-gradient(135deg, hsl(267 86% 68% / .05), transparent 34%, hsl(189 82% 55% / .04));
    background-size: 140% 140%, 180% 180%;
    animation: charo-panel-aura 9s ease-in-out infinite;
  }
  .charo-panel-shell::after {
    content: "";
    position: absolute;
    inset: 0;
    z-index: 2;
    pointer-events: none;
    border-radius: inherit;
    padding: 1px;
    background:
      conic-gradient(
        from var(--charo-border-angle),
        transparent 0deg,
        transparent 64deg,
        hsl(267 86% 70% / .18) 88deg,
        hsl(267 86% 74% / .76) 112deg,
        hsl(188 86% 62% / .5) 132deg,
        transparent 162deg,
        transparent 360deg
      );
    mask: linear-gradient(#000 0 0) content-box, linear-gradient(#000 0 0);
    mask-composite: exclude;
    -webkit-mask: linear-gradient(#000 0 0) content-box, linear-gradient(#000 0 0);
    -webkit-mask-composite: xor;
    animation: charo-border-glow 7s linear infinite;
  }
  .charo-panel-shell > * {
    position: relative;
    z-index: 3;
  }
  @keyframes charo-typing {
    0%, 60%, 100% { transform: translateY(0); opacity: .35; }
    30% { transform: translateY(-3px); opacity: 1; }
  }
  .charo-typing-dot {
    animation: charo-typing 1.2s ease-in-out infinite;
  }
`;

// Three bouncing dots shown in the assistant slot between hitting send and the
// first token, so a pending response never reads as dead air.
function TypingDots() {
  return (
    <span className="flex items-center gap-1 py-1" role="status" aria-label="Charo is thinking">
      {[0, 160, 320].map((d) => (
        <span
          key={d}
          className="charo-typing-dot h-1.5 w-1.5 rounded-full bg-muted-foreground/70"
          style={{ animationDelay: `${d}ms` }}
        />
      ))}
    </span>
  );
}

export function CharoPanel({
  open,
  expanded,
  onClose,
  onExpand,
  onCollapse,
  stream,
  mascotState,
}: {
  open: boolean;
  /** When true the panel renders as a large centered modal instead of the dock. */
  expanded: boolean;
  onClose: () => void;
  onExpand: () => void;
  onCollapse: () => void;
  stream: Stream;
  mascotState: CharoState;
}) {
  const { messages, busy, send, stop, reset, confirmRun, confirmCancel,
          startActivity, submitActivity, cancelActivity } = stream;
  const [text, setText] = useState("");
  const threadRef = useRef<HTMLDivElement>(null);

  // Auto-scroll the thread on new content, and when toggling dock <-> expanded.
  useEffect(() => {
    threadRef.current?.scrollTo({ top: threadRef.current.scrollHeight });
  }, [messages, expanded]);

  const onSend = () => {
    if (!text.trim() || busy) return;
    send(text);
    setText("");
  };

  const mascot = MASCOT[mascotState];
  const canSend = !busy && !!text.trim();

  // Header: title + reset / expand-collapse / close.
  const header = (
    <div className="flex items-center justify-between gap-3 border-b border-border bg-secondary/20 px-4 py-3">
      <div className="truncate text-sm font-semibold">Charo</div>
      <div className="flex items-center gap-1">
        <Button variant="ghost" size="icon" title="Clear conversation" onClick={reset}>
          <RotateCcw className="h-4 w-4" />
        </Button>
        {expanded ? (
          <Button variant="ghost" size="icon" title="Collapse" onClick={onCollapse}>
            <Minimize2 className="h-4 w-4" />
          </Button>
        ) : (
          <Button variant="ghost" size="icon" title="Expand" onClick={onExpand}>
            <Maximize2 className="h-4 w-4" />
          </Button>
        )}
        <Button variant="ghost" size="icon" title="Close" onClick={onClose}>
          <X className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );

  const thread = (
    <div
      ref={threadRef}
      className={cn(
        "w-full flex-1 space-y-3 overflow-y-auto",
        expanded ? "h-full px-5 py-4" : "mx-auto max-w-3xl px-4 py-3",
      )}
    >
      {messages.length === 0 && (
        <div className="flex min-h-full flex-col items-center justify-center gap-4 px-2 py-6">
          <p className="text-sm text-muted-foreground">Hey. What are we getting into?</p>
          <div className="w-full max-w-sm">
            <ActivityCards onPick={(a) => startActivity(a.id)} />
          </div>
          <p className="text-xs text-muted-foreground/70">…or just start typing to chat with me.</p>
        </div>
      )}
      {messages.map((m) => {
        const content = m.role === "assistant" ? m.content.replace(/^\s+/, "") : m.content;

        if (m.workflowActivityId) {
          ensureActivitiesRegistered();
          const activity = getActivity(m.workflowActivityId);
          if (!activity) return null;
          return (
            <div key={m.id} className="w-full">
              <WorkflowCard
                activity={activity}
                onRun={(args) => submitActivity(m.id, activity.id, args)}
                onCancel={() => cancelActivity(m.id)}
              />
            </div>
          );
        }

        if (m.role === "assistant") {
          const hasTrace = m.trace !== undefined || m.tracePending;
          const hasLiveBench = (m.liveSteps?.length ?? 0) > 0 && (m.toolResults?.length ?? 0) === 0;
          const hasLiveCaps = (m.liveCapabilities?.length ?? 0) > 0 && (m.toolResults?.length ?? 0) === 0;
          const hasToolResults = (m.toolResults?.length ?? 0) > 0;
          const showBubble = !!content || !!m.image || !!m.error || !m.streaming;
          if (!showBubble && !hasTrace && !hasLiveBench && !hasLiveCaps && !hasToolResults && !m.pendingConfirm) {
            // Streaming but nothing to show yet (waiting on the first token or a
            // tool's first progress event): show the typing indicator, not dead air.
            if (!m.streaming) return null;
            return (
              <div key={m.id} className="flex flex-col items-start">
                <div className="w-fit rounded-lg rounded-tl-sm bg-muted px-3 py-2">
                  <TypingDots />
                </div>
              </div>
            );
          }
          return (
            <div key={m.id} className="flex flex-col items-start space-y-2">
              {showBubble && (
                <div className="w-fit max-w-[min(46rem,100%)] whitespace-pre-wrap rounded-lg rounded-tl-sm bg-muted px-3 py-2 text-sm text-foreground">
                  {m.image && (
                    /* eslint-disable-next-line @next/next/no-img-element */
                    <img src={m.image} alt="attachment" className="mb-1 max-h-40 rounded-md" />
                  )}
                  {content}
                  {m.error && <span className="text-destructive">{m.error}</span>}
                </div>
              )}
              {hasTrace && (
                <div className="w-full">
                  <TraceCard trace={m.trace} pending={m.tracePending} />
                </div>
              )}
              {hasLiveBench && (
                <div className="w-full">
                  <BenchResultCard data={{ steps: m.liveSteps }} />
                </div>
              )}
              {hasLiveCaps && (
                <div className="w-full">
                  <CapabilityResultCard data={{ modelName: undefined, tests: m.liveCapabilities }} />
                </div>
              )}
              {m.toolResults?.map((tr, i) => {
                const Renderer = resultRenderer(tr.type);
                return <div key={i} className="w-full"><Renderer data={tr.data} /></div>;
              })}
              {m.pendingConfirm && (
                <div className="w-full">
                  <ConfirmCard
                    pending={m.pendingConfirm}
                    disabled={busy}
                    onRun={() => confirmRun(m.id)}
                    onCancel={() => confirmCancel(m.id)}
                  />
                </div>
              )}
            </div>
          );
        }

        return (
          <div key={m.id} className="flex flex-col items-end">
            <div
              className={cn(
                "whitespace-pre-wrap rounded-lg px-3 py-2 text-sm",
                expanded ? "max-w-[82%]" : "max-w-[88%]",
                "rounded-br-sm bg-primary text-primary-foreground",
              )}
            >
              {m.image && (
                /* eslint-disable-next-line @next/next/no-img-element */
                <img src={m.image} alt="attachment" className="mb-1 max-h-40 rounded-md" />
              )}
              {content}
              {m.error && <span className="text-destructive">{m.error}</span>}
            </div>
          </div>
        );
      })}
    </div>
  );

  const expandedMascot = expanded && (
    /* eslint-disable-next-line @next/next/no-img-element */
    <img
      src={mascot}
      alt=""
      draggable={false}
      className="pointer-events-none absolute -top-32 -right-4 z-[5] hidden h-44 w-auto select-none drop-shadow-2xl sm:block lg:-top-36 lg:-right-5 lg:h-48"
    />
  );

  const composer = (
    <div
      className={cn(
        "w-full space-y-2 border-t border-border",
        expanded ? "px-5 py-4" : "mx-auto max-w-3xl px-4 py-3",
      )}
    >
      <div className="rounded-lg border border-border bg-background/90 p-2 shadow-[inset_0_1px_0_hsl(240_5%_100%/0.03)] focus-within:ring-1 focus-within:ring-ring">
        <div className="flex items-end gap-2">
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                if (!busy) onSend();
              }
            }}
            rows={expanded ? 3 : 2}
            placeholder="Message Charo…"
            className="min-h-11 flex-1 resize-none border-0 bg-transparent px-2 py-1.5 text-sm leading-5 placeholder:text-muted-foreground/80 focus-visible:outline-none disabled:opacity-50"
          />
          <div className="flex shrink-0 items-center gap-1">
            <ActivityMenu onPick={(a) => startActivity(a.id)} />
            {busy ? (
              <Button
                size="icon"
                variant="secondary"
                className="h-9 w-9 shrink-0"
                title="Stop"
                onClick={stop}
              >
                <Square className="h-3.5 w-3.5 fill-current" />
              </Button>
            ) : (
              <Button
                size="icon"
                className={cn(
                  "h-9 w-9 shrink-0 disabled:opacity-100",
                  !canSend && "bg-secondary text-muted-foreground hover:bg-secondary",
                )}
                title="Send"
                onClick={onSend}
                disabled={!canSend}
              >
                <Send className="h-4 w-4" />
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );

  // Expanded: large centered modal (Radix gives backdrop, focus-trap, scroll
  // lock, and Esc). Collapse returns to the dock without losing conversation or
  // draft state.
  if (expanded) {
    return (
      <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/60 backdrop-blur-sm data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
          <Dialog.Content
            aria-describedby={undefined}
            onEscapeKeyDown={(e) => e.preventDefault()}
            onInteractOutside={(e) => e.preventDefault()}
            onPointerDownOutside={(e) => e.preventDefault()}
            className={cn(
              "fixed left-1/2 top-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 focus:outline-none",
              "h-[min(46rem,88vh)] w-[min(64rem,94vw)] max-sm:h-[100dvh] max-sm:w-full max-sm:max-w-none",
              "data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
            )}
          >
            <Dialog.Title className="sr-only">Charo</Dialog.Title>
            {expandedMascot}
            <div className="charo-panel-shell relative z-10 flex h-full w-full flex-col overflow-hidden rounded-lg border border-violet-300/15 bg-card shadow-[0_18px_56px_hsl(267_86%_12%/0.28)] ring-1 ring-violet-200/10 max-sm:rounded-none">
              <style>{PANEL_CSS}</style>
              {header}
              <div className="flex min-h-0 flex-1 flex-col">
                {thread}
                {composer}
              </div>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    );
  }

  // Docked: the corner chat window with the large mascot peeking over it.
  if (!open) return null;
  return (
    <div className="charo-panel-frame fixed bottom-4 right-4 isolate z-[60] h-[34rem] max-h-[80vh] w-[27rem] max-w-[calc(100vw-1rem)] sm:bottom-8 sm:right-8">
      <style>{PANEL_CSS}</style>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={mascot}
        alt=""
        draggable={false}
        className="pointer-events-none absolute -top-32 -right-4 z-[5] hidden h-44 w-auto select-none drop-shadow-2xl sm:block lg:-top-36 lg:-right-5 lg:h-48"
      />
      <div
        className="charo-panel-shell relative z-10 flex h-full flex-col overflow-hidden rounded-lg border border-violet-300/15 bg-card shadow-[0_18px_56px_hsl(267_86%_12%/0.28)] ring-1 ring-violet-200/10"
        role="dialog"
        aria-label="Charo"
      >
        {header}
        {thread}
        {composer}
      </div>
    </div>
  );
}
