"use client";

import { useState } from "react";
import { ArrowUp, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import type { PlaygroundSession } from "./playground";
import { MODES } from "./ui";

const CARDS: Record<string, { blurb: string; example: string }> = {
  compare: { blurb: "Talk to one model, or add up to four and compare their answers side by side.", example: "Which model writes the clearest release notes?" },
  image: { blurb: "Generate images from a prompt, then iterate on size, steps and seed.", example: "Check a new image backend end to end" },
  router: { blurb: "See which model auto would pick for a prompt, and try new weights before applying them.", example: "Why did this request go to the small model?" },
  verdicts: { blurb: "Ask yes/no, choice or score questions about text or JSON and get calibrated answers.", example: "Triage a support ticket into a team and urgency" },
  search: { blurb: "Run a web search through one of the gateway's search tools and see the results a client gets.", example: "Check the SearXNG tool answers before pointing an app at it" },
};

/** What a new session shows until the person picks a mode or just starts typing. */
export function NewSession({ onPick, onQuickStart, onAssistant }: {
  onPick: (mode: PlaygroundSession["mode"]) => void;
  onQuickStart: (text: string) => void;
  onAssistant: (activity: string) => void;
}) {
  const [text, setText] = useState("");
  const start = () => { if (text.trim()) onQuickStart(text); };
  return (
    <div className="h-full overflow-y-auto px-4 py-10 md:py-16">
      <div className="mx-auto flex max-w-3xl flex-col gap-7">
        <div className="space-y-2 text-center">
          <h2 className="text-2xl font-semibold tracking-tight md:text-[30px]">What do you want to try?</h2>
          <p className="text-sm text-muted-foreground">Pick a starting point. You can switch modes at any time, and the session is saved in this browser.</p>
        </div>

        <div className="grid gap-3 sm:grid-cols-2">
          {MODES.map(({ mode, label, icon: Icon }) => (
            <button
              key={mode}
              type="button"
              onClick={() => onPick(mode)}
              className="flex gap-3.5 rounded-2xl border border-border bg-card/60 p-4 text-left transition-colors hover:border-muted-foreground/50 hover:bg-card focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring md:p-[18px]"
            >
              <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-[10px] border border-border"><Icon className="h-[18px] w-[18px]" aria-hidden="true" /></span>
              <span className="flex flex-col gap-1">
                <span className="text-[15px] font-semibold">{label}</span>
                <span className="text-[13px] leading-relaxed text-secondary-foreground">{CARDS[mode].blurb}</span>
                <span className="text-xs italic text-muted-foreground">“{CARDS[mode].example}”</span>
              </span>
            </button>
          ))}
        </div>

        <div className="space-y-2.5">
          <div className="flex items-center gap-3 text-xs text-muted-foreground"><span className="h-px flex-1 bg-border" />or just start typing<span className="h-px flex-1 bg-border" /></div>
          <div className="rounded-2xl border border-border bg-card focus-within:border-muted-foreground/50">
            <textarea
              aria-label="Quick start message"
              rows={2}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); start(); } }}
              placeholder="Ask anything. Automatic routing picks the model…"
              className="block w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"
            />
            <div className="flex items-center justify-between px-2.5 pb-2.5 pt-2">
              <span className="px-1.5 font-mono text-xs text-muted-foreground">model: auto</span>
              <Button size="icon" className="h-9 w-9 rounded-[10px]" aria-label="Start chat" title="Start chat" disabled={!text.trim()} onClick={start}><ArrowUp className="h-4 w-4" /></Button>
            </div>
          </div>
        </div>

        <div className="flex flex-col gap-3 rounded-xl border border-dashed border-border p-4 sm:flex-row sm:items-center">
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-dashed border-border"><Sparkles className="h-4 w-4" aria-hidden="true" /></span>
          <span className="flex-1 text-[13px] text-secondary-foreground">New model deployed? The Assistant can check its capabilities or run a quick benchmark.</span>
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={() => onAssistant("test_capabilities")}>Test capabilities</Button>
            <Button variant="outline" size="sm" onClick={() => onAssistant("benchmark")}>Benchmark</Button>
          </div>
        </div>
      </div>
    </div>
  );
}
