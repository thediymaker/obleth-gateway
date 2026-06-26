"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { Send, X, ImagePlus, RotateCcw, Trash2, ChevronDown, Check } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import type { ModelRoute } from "@/lib/obleth";
import { TraceCard } from "./trace-card";
import type { useCharoStream } from "./use-charo-stream";
import type { CharoState } from "./sprite";

type Stream = ReturnType<typeof useCharoStream>;

interface Preset {
  label: string;
  prompt: string;
}

// A model can take image input either natively (`supports_vision`) or via the
// gateway's vision boon, which relays images to a describer model. Either way
// Charo should offer the image attachment.
function hasVision(m: ModelRoute | undefined): boolean {
  return !!m && (m.supports_vision || m.boons.includes("vision"));
}

function presetsFor(m: ModelRoute | undefined): Preset[] {
  if (!m) return [];
  const out: Preset[] = [
    { label: "Quick ping", prompt: "Reply with a short sentence to confirm you're responding." },
  ];
  if (m.tool_servers.length > 0 || m.supports_function_calling) {
    out.push({
      label: "Trigger tools / search",
      prompt:
        "Search the web for a surprising fact about octopuses and summarise what you find, citing your source.",
    });
  }
  if (m.supports_response_schema || m.boons.includes("structured_output")) {
    out.push({
      label: "Force JSON",
      prompt:
        'Reply with ONLY this JSON object and nothing else: {"status":"ok","gateway":"obleth"}',
    });
  }
  if (hasVision(m)) {
    out.push({
      label: "Describe image",
      prompt: "Describe the attached image in detail.",
    });
  }
  return out;
}

function configuredBoons(m: ModelRoute | undefined): string[] {
  if (!m) return [];
  const b = new Set<string>(m.boons);
  if (m.supports_vision) b.add("vision");
  if (m.tool_servers.length > 0 || m.supports_function_calling) b.add("tool_loop");
  if (m.supports_response_schema) b.add("structured_output");
  return [...b];
}

const MASCOT: Record<CharoState, string> = {
  idle: "/charo/charo-dark-idle.png",
  held: "/charo/charo-dark-idle.png",
  thinking: "/charo/charo-dark-thinking.png",
  result: "/charo/charo-dark-result.png",
  error: "/charo/charo-dark-error.png",
};

export function CharoPanel({
  open,
  onClose,
  stream,
  mascotState,
}: {
  open: boolean;
  onClose: () => void;
  stream: Stream;
  mascotState: CharoState;
}) {
  const { messages, busy, send, reset } = stream;
  const [models, setModels] = useState<ModelRoute[]>([]);
  const [modelId, setModelId] = useState("");
  const [text, setText] = useState("");
  const [image, setImage] = useState<string | undefined>();
  const fileRef = useRef<HTMLInputElement>(null);
  const threadRef = useRef<HTMLDivElement>(null);

  const selected = useMemo(
    () => models.find((m) => m.id === modelId),
    [models, modelId],
  );

  // Load the enabled model list once the panel first opens.
  useEffect(() => {
    if (!open || models.length > 0) return;
    let cancelled = false;
    fetch("/api/live/models")
      .then((r) => (r.ok ? r.json() : []))
      .then((list: ModelRoute[]) => {
        if (cancelled) return;
        const enabled = list.filter((m) => m.enabled);
        setModels(enabled);
        if (enabled[0]) setModelId(enabled[0].id);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [open, models.length]);

  // Auto-scroll the thread on new content.
  useEffect(() => {
    threadRef.current?.scrollTo({ top: threadRef.current.scrollHeight });
  }, [messages]);

  if (!open) return null;

  const onSend = () => {
    if (!selected || busy) return;
    send(selected.model_name, text, image);
    setText("");
    setImage(undefined);
  };

  const onPickImage = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-picking the same file after removing it
    if (!file) return;
    // Cap the attachment: it's base64-inlined into the JSON request body, so a
    // large image balloons the payload (and may be rejected upstream).
    const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
    if (file.size > MAX_IMAGE_BYTES) {
      window.alert("Image is too large (max 8 MB).");
      return;
    }
    const r = new FileReader();
    r.onload = () => setImage(String(r.result));
    r.readAsDataURL(file);
  };

  const boons = configuredBoons(selected);
  const presets = presetsFor(selected);
  const mascot = MASCOT[mascotState];

  return (
    <div className="charo-panel-frame fixed bottom-4 right-4 isolate z-[60] h-[34rem] max-h-[80vh] w-[27rem] max-w-[calc(100vw-1rem)] sm:bottom-8 sm:right-8">
      <style>{`
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
      `}</style>
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
        aria-label="Charo model tester"
      >
        {/* header */}
        <div className="flex items-center justify-between gap-3 border-b border-border bg-secondary/20 px-4 py-3">
          <div className="min-w-0">
            <div className="truncate text-sm font-semibold">Charo - model tester</div>
            <div className="text-xs text-muted-foreground">
              Chats hit the real gateway as the internal tenant.
            </div>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              title="Clear conversation"
              onClick={reset}
              disabled={busy}
            >
              <RotateCcw className="h-4 w-4" />
            </Button>
            <Button variant="ghost" size="icon" title="Close" onClick={onClose}>
              <X className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {/* model + boons */}
        <div className="space-y-2 border-b border-border px-4 py-3">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              disabled={busy || models.length === 0}
              className="flex h-9 w-full items-center justify-between rounded-md border border-border bg-background px-3 text-sm shadow-sm transition-colors hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            >
              <span className="truncate">
                {selected ? selected.model_name : "No models available"}
              </span>
              <ChevronDown className="ml-2 h-4 w-4 shrink-0 opacity-60" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="start"
            className="z-[70] max-h-72 w-[var(--radix-dropdown-menu-trigger-width)] overflow-y-auto"
          >
            {models.map((m) => (
              <DropdownMenuItem
                key={m.id}
                onSelect={() => setModelId(m.id)}
                className="cursor-pointer justify-between gap-2"
              >
                <span className="truncate">{m.model_name}</span>
                {m.id === modelId && (
                  <Check className="h-4 w-4 shrink-0 text-primary" />
                )}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
        <div className="flex flex-wrap gap-1.5">
          {boons.length === 0 ? (
            <span className="text-xs text-muted-foreground">No boons configured</span>
          ) : (
            boons.map((b) => <Badge key={b}>{b}</Badge>)
          )}
        </div>
      </div>

      {/* thread */}
      <div ref={threadRef} className="flex-1 space-y-3 overflow-y-auto px-4 py-3">
        {messages.length === 0 && (
          <p className="text-center text-xs text-muted-foreground">
            Pick a model and send a message to test it. Charo runs it through the
            gateway so every boon fires for real - the trace shows what happened.
          </p>
        )}
        {messages.map((m) => (
          <div
            key={m.id}
            className={cn("flex flex-col", m.role === "user" ? "items-end" : "items-start")}
          >
            <div
              className={cn(
                "max-w-[88%] whitespace-pre-wrap rounded-lg px-3 py-2 text-sm",
                m.role === "user"
                  ? "rounded-br-sm bg-primary text-primary-foreground"
                  : "rounded-tl-sm bg-muted text-foreground",
              )}
            >
              {m.image && (
                /* eslint-disable-next-line @next/next/no-img-element */
                <img
                  src={m.image}
                  alt="attachment"
                  className="mb-1 max-h-40 rounded-md"
                />
              )}
              {m.content || (m.streaming ? "..." : "")}
              {m.error && <span className="text-destructive">{m.error}</span>}
            </div>
            {m.role === "assistant" && (m.trace !== undefined || m.tracePending) && (
              <div className="w-full">
                <TraceCard
                  trace={m.trace}
                  pending={m.tracePending}
                  configured={boons}
                />
              </div>
            )}
          </div>
        ))}
      </div>

      {/* composer */}
      <div className="space-y-2 border-t border-border px-4 py-3">
        {presets.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {presets.map((p) => (
              <button
                key={p.label}
                type="button"
                onClick={() => setText(p.prompt)}
                disabled={busy}
                className="rounded-full border border-border px-2.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground disabled:opacity-50"
              >
                {p.label}
              </button>
            ))}
          </div>
        )}

        {image && (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src={image} alt="to send" className="h-10 w-10 rounded object-cover" />
            <span>image attached</span>
            <button
              type="button"
              onClick={() => setImage(undefined)}
              className="text-destructive hover:underline"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        )}

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
            rows={2}
            placeholder={selected ? `Message ${selected.model_name}...` : "Select a model..."}
            // Stay enabled while a response streams (only require a selected
            // model) so the input keeps focus after Enter; Enter is ignored
            // mid-stream via the guard above, and the Send button is disabled.
            disabled={!selected}
            className="flex-1 resize-none rounded-md border border-border bg-background px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-50"
          />
          <div className="flex flex-col gap-1">
            {hasVision(selected) && (
              <>
                <input
                  ref={fileRef}
                  type="file"
                  accept="image/*"
                  className="hidden"
                  onChange={onPickImage}
                />
                <Button
                  variant="outline"
                  size="icon"
                  title="Attach image (vision)"
                  onClick={() => fileRef.current?.click()}
                  disabled={busy}
                >
                  <ImagePlus className="h-4 w-4" />
                </Button>
              </>
            )}
            <Button
              size="icon"
              title="Send"
              onClick={onSend}
              disabled={busy || !selected || (!text.trim() && !image)}
            >
              <Send className="h-4 w-4" />
            </Button>
          </div>
        </div>
      </div>
      </div>
    </div>
  );
}
