"use client";

import { useMemo, useState } from "react";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button } from "@/components/ui/button";
import { useCharoContext } from "./charo-context";
import { resultRenderer } from "./results/registry";
import { BenchResultCard } from "./results/bench-result-card";
import { CharoSettingsForm } from "@/components/settings-form";
import type { CharoSettingsView, ModelRoute } from "@/lib/obleth";
import type { BenchResult } from "@/lib/charo/bench/types";

export function CharoWorkspace({ settings, models }: { settings: CharoSettingsView | null; models: ModelRoute[] }) {
  const stream = useCharoContext();
  const { messages, busy, send, runToolDirect } = stream;
  const [text, setText] = useState("");
  const [subject, setSubject] = useState(models.find((m) => m.enabled)?.model_name ?? "");
  const brain = settings?.brain_model ?? null;

  const runs = useMemo(
    () => messages.flatMap((m) => (m.toolResults ?? []).filter((t) => t.type === "bench_result").map((t) => t.data as BenchResult)),
    [messages],
  );

  const onSend = () => {
    if (!text.trim() || busy) return;
    // With a brain, chat drives tools; without, fall back to the plain tester.
    send(brain ?? subject, text);
    setText("");
  };

  return (
    <Tabs defaultValue="chat">
      <TabsList>
        <TabsTrigger value="chat">Chat</TabsTrigger>
        <TabsTrigger value="runs">Runs{runs.length ? ` (${runs.length})` : ""}</TabsTrigger>
        <TabsTrigger value="settings">Settings</TabsTrigger>
      </TabsList>

      <TabsContent value="chat">
        <div className="grid gap-4 lg:grid-cols-[1fr_16rem]">
          <div className="space-y-3">
            {!brain && (
              <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-700 dark:text-amber-300">
                No brain model assigned — Charo is in tester mode. Assign one in the Settings tab to enable tool use from chat.
              </p>
            )}
            <div className="min-h-[24rem] space-y-3 rounded-lg border border-border bg-card p-4">
              {messages.length === 0 && <p className="text-sm text-muted-foreground">Start a conversation or run a tool from the rail.</p>}
              {messages.map((m) => (
                <div key={m.id} className={m.role === "user" ? "text-right" : ""}>
                  <div className="inline-block max-w-[80%] whitespace-pre-wrap rounded-lg px-3 py-2 text-sm" style={{ background: m.role === "user" ? "var(--primary)" : "var(--muted)" }}>
                    {m.content || (m.streaming ? "..." : "")}
                    {m.error && <span className="text-destructive">{m.error}</span>}
                  </div>
                  {(m.liveSteps?.length ?? 0) > 0 && (m.toolResults?.length ?? 0) === 0 && (
                    <BenchResultCard data={{ modelName: subject, steps: m.liveSteps }} />
                  )}
                  {m.toolResults?.map((tr, i) => {
                    const R = resultRenderer(tr.type);
                    return <R key={i} data={tr.data} />;
                  })}
                </div>
              ))}
            </div>
            <div className="flex items-end gap-2">
              <textarea
                value={text}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); onSend(); } }}
                rows={2}
                placeholder={brain ? "Ask Charo to benchmark a model..." : "Message the model under test..."}
                className="flex-1 resize-none rounded-md border border-border bg-background px-3 py-2 text-sm"
              />
              <Button onClick={onSend} disabled={busy || !text.trim()}>Send</Button>
            </div>
          </div>

          <aside className="space-y-3">
            <div className="rounded-lg border border-border p-3">
              <div className="text-xs font-medium text-muted-foreground">Tools</div>
              <div className="mt-2 space-y-2">
                <select value={subject} onChange={(e) => setSubject(e.target.value)} className="h-8 w-full rounded-md border border-border bg-background px-2 text-xs">
                  {models.filter((m) => m.enabled).map((m) => <option key={m.id} value={m.model_name}>{m.model_name}</option>)}
                </select>
                <Button
                  size="sm"
                  className="w-full"
                  disabled={busy || !subject}
                  onClick={() => runToolDirect("run_benchmark", { model: subject })}
                >
                  Run capacity benchmark
                </Button>
              </div>
            </div>
          </aside>
        </div>
      </TabsContent>

      <TabsContent value="runs">
        <div className="space-y-4">
          {runs.length === 0 && <p className="text-sm text-muted-foreground">No runs yet this session.</p>}
          {runs.map((r, i) => <BenchResultCard key={i} data={r} />)}
        </div>
      </TabsContent>

      <TabsContent value="settings">
        <CharoSettingsForm settings={settings} models={models} />
      </TabsContent>
    </Tabs>
  );
}
