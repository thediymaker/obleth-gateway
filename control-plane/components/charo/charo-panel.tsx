"use client";

import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Send, Square, X, RotateCcw, Maximize2, Minimize2, Paperclip } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { TraceCard } from "./trace-card";
import { resultRenderer } from "./results/registry";
import { BenchResultCard } from "./results/bench-result-card";
import { CapabilityResultCard } from "./results/capability-result-card";
import { ConfirmCard } from "./results/confirm-card";
import { WorkflowCard } from "./workflow-card";
import { ActivityCards } from "./activity-launcher";
import { ensureActivitiesRegistered, getActivity } from "@/lib/charo/activities";
import { coalesceDocsResults } from "@/lib/charo/docs/coalesce-docs-results";
import { CharoMarkdown } from "./markdown";
import { MicroLabel } from "./rail";
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
          startActivity, submitActivity, cancelActivity, activeTarget, clearTarget } = stream;
  const [text, setText] = useState("");
  // Pending image attachment (data URL) for the next send — set via the
  // paperclip picker or by dropping a file anywhere on the panel.
  const [image, setImage] = useState<string | null>(null);
  const [attachError, setAttachError] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const dragDepth = useRef(0);
  const threadRef = useRef<HTMLDivElement>(null);

  const attachFromFiles = (files: FileList | null) => {
    const file = files?.[0];
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      setAttachError("Only images can be attached.");
      return;
    }
    if (file.size > 6 * 1024 * 1024) {
      setAttachError("Image is too large (max 6 MB).");
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      setImage(String(reader.result));
      setAttachError(null);
    };
    reader.readAsDataURL(file);
  };

  // Drag-and-drop an image anywhere on the panel. dragenter/leave fire on every
  // child, so a depth counter decides when the pointer actually left the panel.
  const hasFiles = (e: React.DragEvent) => Array.from(e.dataTransfer.types).includes("Files");
  const dropProps = {
    onDragEnter: (e: React.DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      dragDepth.current += 1;
      setDragOver(true);
    },
    onDragOver: (e: React.DragEvent) => {
      if (hasFiles(e)) e.preventDefault();
    },
    onDragLeave: () => {
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (dragDepth.current === 0) setDragOver(false);
    },
    onDrop: (e: React.DragEvent) => {
      e.preventDefault();
      dragDepth.current = 0;
      setDragOver(false);
      // A drop on a dedicated dropzone (e.g. the vision test's image field)
      // is handled there — don't also attach it to the composer.
      if ((e.target as HTMLElement | null)?.closest?.("[data-charo-dropzone]")) return;
      attachFromFiles(e.dataTransfer.files);
    },
  };

  // Always jump to the bottom when toggling dock <-> expanded (the container
  // remounts, losing its scroll position).
  useEffect(() => {
    threadRef.current?.scrollTo({ top: threadRef.current.scrollHeight });
  }, [expanded]);

  // Follow new content only while the reader is already near the bottom, so
  // scrolling up to read mid-stream isn't yanked back down by the next token.
  useEffect(() => {
    const el = threadRef.current;
    if (!el) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) {
      el.scrollTo({ top: el.scrollHeight });
    }
  }, [messages]);

  const onSend = () => {
    if ((!text.trim() && !image) || busy) return;
    send(text, image ?? undefined);
    setText("");
    setImage(null);
  };

  const mascot = MASCOT[mascotState];
  const canSend = !busy && (!!text.trim() || !!image);

  // Shown while a file drag hovers the panel.
  const dropOverlay = dragOver ? (
    // position/z-index inline: the shell's `> *` rule forces relative z-3 on
    // children and would otherwise override the utility classes.
    <div
      style={{ position: "absolute", zIndex: 20 }}
      className="pointer-events-none inset-2 flex items-center justify-center rounded-lg border-2 border-dashed border-violet-400/60 bg-violet-500/10"
    >
      <p className="text-[13px] font-medium text-violet-700 dark:text-violet-200">Drop image to attach</p>
    </div>
  ) : null;

  // Header: title + reset / expand-collapse / close.
  const header = (
    <div className="flex items-center justify-between gap-3 border-b border-border bg-secondary/20 px-4 py-3">
      <div className="truncate text-sm font-semibold">Gateway Chat</div>
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

  const targetBanner = activeTarget ? (
    <div className="flex items-center justify-between gap-2 border-b border-violet-400/30 bg-violet-500/10 px-4 py-2 text-xs">
      <span className="truncate">Chatting with <span className="font-semibold">{activeTarget}</span></span>
      <button type="button" onClick={clearTarget} className="rounded border border-border px-2 py-0.5 text-muted-foreground hover:bg-accent hover:text-accent-foreground">
        Exit
      </button>
    </div>
  ) : null;

  const thread = (
    <div
      ref={threadRef}
      className={cn(
        "w-full flex-1 space-y-4 overflow-y-auto",
        expanded ? "h-full px-5 py-4" : "mx-auto max-w-3xl px-4 py-3",
      )}
    >
      {messages.length === 0 && (
        <div className="flex min-h-full flex-col items-center justify-center gap-4 px-2 py-6">
          <p className="text-[15px] font-semibold text-foreground">What can I help with?</p>
          <div className="w-full max-w-sm">
            <ActivityCards onPick={(a) => startActivity(a.id)} />
          </div>
          <p className="text-[11.5px] text-muted-foreground/70">…or just start typing.</p>
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

        if (m.showLauncher) {
          return (
            <div key={m.id} className="w-full max-w-sm">
              <ActivityCards onPick={(a) => startActivity(a.id)} />
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
                <MicroLabel className="mb-1 text-violet-600/70 dark:text-violet-400/70">Charo</MicroLabel>
                <TypingDots />
              </div>
            );
          }
          // Children follow ARRIVAL order: tool/live cards land before the
          // answer tokens, so they render above the streaming text — the text
          // grows at the bottom, where the sticky auto-scroll keeps the view.
          return (
            <div key={m.id} className="flex flex-col items-start space-y-2">
              <MicroLabel className="-mb-1 text-violet-600/70 dark:text-violet-400/70">Charo</MicroLabel>
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
              {coalesceDocsResults(m.toolResults ?? []).map((tr, i) => {
                const Renderer = resultRenderer(tr.type);
                return <div key={i} className="w-full"><Renderer data={tr.data} /></div>;
              })}
              {showBubble && (
                <div className="w-full min-w-0">
                  {m.image && (
                    /* eslint-disable-next-line @next/next/no-img-element */
                    <img src={m.image} alt="attachment" className="mb-1.5 max-h-40 rounded-md" />
                  )}
                  {content && <CharoMarkdown text={content} />}
                  {m.error && <p className="mt-1 text-[13px] text-destructive">{m.error}</p>}
                </div>
              )}
              {!content && !m.error && m.streaming && <TypingDots />}
              {hasTrace && (
                <div className="w-full">
                  <TraceCard trace={m.trace} pending={m.tracePending} />
                </div>
              )}
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
                "whitespace-pre-wrap break-words px-3 py-[7px] text-[13.5px] leading-relaxed",
                expanded ? "max-w-[82%]" : "max-w-[88%]",
                "rounded-[14px] rounded-br-[4px] border border-violet-500/30 bg-violet-500/15 text-violet-950 dark:text-violet-50",
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
        {image && (
          <div className="px-1 pb-2 pt-1">
            <div className="relative inline-block">
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img src={image} alt="pending attachment" className="h-14 rounded-md" />
              <button
                type="button"
                title="Remove image"
                onClick={() => setImage(null)}
                className="absolute -right-2 -top-2 flex h-5 w-5 items-center justify-center rounded-full border border-border bg-background text-muted-foreground hover:text-foreground"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          </div>
        )}
        {attachError && <p className="px-1 pb-1 text-[11.5px] text-destructive">{attachError}</p>}
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
            placeholder={activeTarget ? `Message ${activeTarget}…` : "Message Charo…"}
            className="min-h-11 flex-1 resize-none border-0 bg-transparent px-2 py-1.5 text-sm leading-5 placeholder:text-muted-foreground/80 focus-visible:outline-none disabled:opacity-50"
          />
          <div className="flex shrink-0 items-center gap-1">
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              className="hidden"
              onChange={(e) => {
                attachFromFiles(e.target.files);
                e.currentTarget.value = "";
              }}
            />
            <button
              type="button"
              title="Attach image"
              onClick={() => fileInputRef.current?.click()}
              className="flex h-9 w-9 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent/40"
            >
              <Paperclip className="h-4 w-4" />
            </button>
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
            <Dialog.Title className="sr-only">Gateway Chat</Dialog.Title>
            {expandedMascot}
            <div
              {...dropProps}
              className="charo-panel-shell relative z-10 flex h-full w-full flex-col overflow-hidden rounded-lg border border-violet-300/15 bg-card shadow-[0_18px_56px_hsl(267_86%_12%/0.28)] ring-1 ring-violet-200/10 max-sm:rounded-none"
            >
              <style>{PANEL_CSS}</style>
              {dropOverlay}
              {header}
              {targetBanner}
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
        {...dropProps}
        className="charo-panel-shell relative z-10 flex h-full flex-col overflow-hidden rounded-lg border border-violet-300/15 bg-card shadow-[0_18px_56px_hsl(267_86%_12%/0.28)] ring-1 ring-violet-200/10"
        role="dialog"
        aria-label="Gateway Chat"
      >
        {dropOverlay}
        {header}
        {targetBanner}
        {thread}
        {composer}
      </div>
    </div>
  );
}
