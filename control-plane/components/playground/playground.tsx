"use client";

import { useEffect, useState } from "react";
import { z } from "zod";
import { ArrowDownToLine, History, Image as ImageIcon, MessageSquare, Plus, Route, SlidersHorizontal, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { generationSchema } from "@/lib/charo/chat-request";
import { useEnabledModels } from "@/components/charo/use-enabled-models";
import { UnifiedWorkspace } from "./workspaces";
import { RouterWorkspace } from "./router-workspace";
import { ImageWorkspace } from "./image-workspace";
import { migrateLegacySessions } from "./session-migration";
import { cn } from "@/lib/utils";

// Shared by the zod cap below and the Router textarea's `maxLength`
// (router-workspace.tsx) so the two can't drift apart: if the input ever
// accepts more than the schema allows, a session with a long-enough
// `routerPrompt` fails `z.array(sessionSchema).safeParse` on the next load
// and playground.tsx silently replaces *every* saved session with a fresh
// one. The gateway itself only reads the first ~2,000 characters for intent
// classification, but "paste a document you want routed" is the mode's
// headline use case, so the cap is generous rather than tight.
export const ROUTER_PROMPT_MAX_LENGTH = 100_000;

// Exported so a test can assert a session parses/round-trips without
// duplicating the schema, and so its cap can be checked against
// ROUTER_PROMPT_MAX_LENGTH directly rather than by re-deriving it.
export const sessionSchema = z.object({
  id: z.string(), title: z.string(), mode: z.enum(["chat", "compare", "router", "image"]),
  models: z.array(z.string()).min(1).max(4), generation: generationSchema,
  recipients: z.array(z.number().int().min(0).max(3)).optional(),
  // Router mode's form draft — everything here is cheap to carry on the
  // session (not component-local state) so it survives a Chat<->Router
  // toggle, which remounts RouterWorkspace since the two modes render
  // different component types. The weight sliders deliberately do NOT get
  // this treatment: they re-seed from the live settings on every mount by
  // design (see router-workspace.tsx), and preserving in-progress edits
  // across a remount would need to change that seeding logic itself.
  routerPrompt: z.string().max(ROUTER_PROMPT_MAX_LENGTH).optional(),
  routerTenantId: z.string().max(200).optional(),
  routerEffort: z.enum(["low", "medium", "high"]).optional(),
  routerMaxTokens: z.number().int().min(1).max(131_072).optional(),
  routerNeedsFunctionCalling: z.boolean().optional(),
  routerNeedsToolChoice: z.boolean().optional(),
  routerNeedsResponseSchema: z.boolean().optional(),
  // Image mode's form draft. Parameters live on the session so a prompt
  // survives a reload and can be re-run; the produced images deliberately do
  // NOT (see image-workspace.tsx — base64 results would blow the localStorage
  // quota and start breaking saves of the session list itself).
  imageModel: z.string().max(200).optional(),
  imagePrompt: z.string().max(4000).optional(),
  imageNegativePrompt: z.string().max(4000).optional(),
  imageSize: z.string().max(20).optional(),
  imageCount: z.number().int().min(1).max(4).optional(),
  imageSteps: z.number().int().min(1).max(150).optional(),
  imageSeed: z.number().int().min(0).max(4_294_967_295).optional(),
});
export type PlaygroundSession = z.infer<typeof sessionSchema>;
const fresh = (): PlaygroundSession => ({ id: crypto.randomUUID(), title: "Untitled session", mode: "compare", models: ["charo"], generation: { systemPrompt: "" } });

export function Playground({ scope }: { scope: string }) {
  const root = `obleth-playground:${encodeURIComponent(scope)}`;
  const [sessions, setSessions] = useState<PlaygroundSession[]>([]);
  const [active, setActive] = useState("");
  const [storageError, setStorageError] = useState<string | null>(null);
  const [showSessions, setShowSessions] = useState(true);
  const [showSettings, setShowSettings] = useState(false);
  const { models, loading, error, reload } = useEnabledModels();
  useEffect(() => {
    try {
      const parsed = z.array(sessionSchema).max(100).safeParse(JSON.parse(localStorage.getItem(root) ?? "[]"));
      const stored = parsed.success && parsed.data.length ? parsed.data : [fresh()];
      migrateLegacySessions(stored, root, localStorage);
      setSessions(stored); setActive(stored[0].id);
    } catch { const session = fresh(); setSessions([session]); setActive(session.id); setStorageError("Browser storage is unavailable. Sessions will last only until you leave."); }
  }, [root]);
  useEffect(() => {
    if (!sessions.length) return;
    try { localStorage.setItem(root, JSON.stringify(sessions)); }
    catch { setStorageError("Session settings could not be saved. Export important conversations before leaving."); }
  }, [root, sessions]);
  const session = sessions.find((s) => s.id === active);
  const update = (patch: Partial<PlaygroundSession>) => setSessions((all) => all.map((s) => s.id === active ? { ...s, ...patch } : s));
  const create = () => {
    const next = fresh(); setSessions((all) => [next, ...all]); setActive(next.id);
  };
  const remove = (id: string) => {
    const remaining = sessions.filter((s) => s.id !== id);
    const next = remaining.length ? remaining : [fresh()];
    setSessions(next);
    if (id === active) setActive(next[0].id);
    // Delay cleanup until the outgoing workspace's unmount save has finished.
    setTimeout(() => {
      try { Object.keys(localStorage).filter((k) => k.startsWith(`${root}:${id}:`)).forEach((k) => localStorage.removeItem(k)); }
      catch { /* storage warning already displayed */ }
    }, 0);
  };
  const exportSession = () => {
    const conversations: Record<string, unknown> = {};
    try {
      Object.keys(localStorage).filter((k) => k.startsWith(`${root}:${active}:`)).forEach((k) => { conversations[k.slice(root.length + active.length + 2)] = JSON.parse(localStorage.getItem(k) ?? "null"); });
    } catch { setStorageError("Only the open conversations could be exported; browser storage is unavailable."); }
    const live: Record<string, unknown> = {};
    window.dispatchEvent(new CustomEvent("playground-export", { detail: live }));
    Object.entries(live).filter(([k]) => k.startsWith(`${root}:${active}:`)).forEach(([k, value]) => { conversations[k.slice(root.length + active.length + 2)] = value; });
    const url = URL.createObjectURL(new Blob([JSON.stringify({ session, conversations }, null, 2)], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = "playground-session.json"; link.click(); URL.revokeObjectURL(url);
  };
  if (!session) return <p className="p-6 text-sm text-muted-foreground">Loading Playground…</p>;
  return (
    <div className="flex h-full min-h-[36rem] flex-col">
      {storageError && <p role="alert" className="px-4 py-2 text-xs text-amber-600">{storageError}</p>}
      {error && <div role="alert" className="flex items-center gap-3 px-4 py-2 text-sm text-destructive">{error}<Button variant="outline" size="sm" onClick={reload}>Retry loading models</Button></div>}
      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        {showSessions && <aside className="flex shrink-0 flex-col gap-3 border-b border-border bg-secondary/10 p-3 md:w-52 md:border-b-0 md:border-r">
          <Button variant="outline" size="sm" onClick={create} disabled={sessions.length >= 50}><Plus className="mr-2 h-4 w-4" />New session</Button>

          <div className="max-h-40 min-h-0 flex-1 space-y-1 overflow-y-auto md:max-h-none">{sessions.map((s) => <div key={s.id} className={cn("group flex w-full items-center rounded-md", active === s.id ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-accent")}>
            <button onClick={() => setActive(s.id)} aria-current={active === s.id ? "true" : undefined} className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-2 text-left text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
              <MessageSquare className="h-3.5 w-3.5 shrink-0" /><span className="truncate">{s.title}</span>
            </button>
            <Button variant="ghost" size="icon" className="mr-1 h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive" title="Delete session" aria-label={`Delete session: ${s.title}`} onClick={() => remove(s.id)}><Trash2 className="h-3.5 w-3.5" /></Button>
          </div>)}</div>
          <p className="hidden text-[11px] text-muted-foreground md:block">Sessions are saved in this browser for your account.</p>
        </aside>}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {/* One control row for the whole page. There used to be a second header above this one carrying a duplicate "Playground" title and a second panel toggle; the app shell already names the page and already owns the navigation toggle. */}
          <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2">
            <Button variant="ghost" size="icon" title="Toggle session list" aria-label="Toggle session list" aria-expanded={showSessions} onClick={() => setShowSessions(!showSessions)}><History className="h-4 w-4" /></Button>
            {!showSessions && <Button variant="ghost" size="icon" title="New session" onClick={create} disabled={sessions.length >= 50}><Plus className="h-4 w-4" /></Button>}
            <input aria-label="Session name" maxLength={100} className="min-w-0 flex-1 bg-transparent text-sm outline-none focus:ring-1 focus:ring-ring" value={session.title} onChange={(e) => update({ title: e.target.value })} />
            <div role="group" aria-label="Playground mode" className="flex shrink-0 items-center gap-0.5 rounded-md border border-border p-0.5">
              <Button type="button" variant={session.mode === "chat" || session.mode === "compare" ? "secondary" : "ghost"} size="sm" aria-pressed={session.mode === "chat" || session.mode === "compare"} onClick={() => update({ mode: "compare" })}><MessageSquare className="mr-1.5 h-3.5 w-3.5" />Chat</Button>
              <Button type="button" variant={session.mode === "router" ? "secondary" : "ghost"} size="sm" aria-pressed={session.mode === "router"} onClick={() => update({ mode: "router" })}><Route className="mr-1.5 h-3.5 w-3.5" />Router</Button>
              <Button type="button" variant={session.mode === "image" ? "secondary" : "ghost"} size="sm" aria-pressed={session.mode === "image"} onClick={() => update({ mode: "image" })}><ImageIcon className="mr-1.5 h-3.5 w-3.5" />Image</Button>
            </div>
            {/* Every mode has request parameters worth hiding until asked for, and every mode puts them in the same place. */}
            <Button variant="outline" size="sm" onClick={() => setShowSettings(!showSettings)} aria-expanded={showSettings}><SlidersHorizontal className="mr-2 h-4 w-4" />Parameters</Button>
            <Button variant="ghost" size="icon" title="Export saved session" onClick={exportSession}><ArrowDownToLine className="h-4 w-4" /></Button>
          </div>
          {showSettings && (session.mode === "chat" || session.mode === "compare") && <div className="grid gap-3 border-b border-border bg-secondary/20 p-4 sm:grid-cols-[1fr_9rem_10rem]">
            <label className="text-xs font-medium">System prompt<textarea aria-label="System prompt" value={session.generation.systemPrompt} maxLength={32000} onChange={(e) => update({ generation: { ...session.generation, systemPrompt: e.target.value } })} className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 text-sm" rows={2} placeholder="Instructions for direct chat and comparison" /></label>
            <label className="text-xs font-medium">Temperature<Input aria-label="Temperature" className="mt-1" type="number" min={0} max={2} step={0.1} placeholder="Model default" value={session.generation.temperature ?? ""} onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (n >= 0 && n <= 2)) update({ generation: { ...session.generation, temperature: e.target.value ? n : undefined } }); }} /></label>
            <label className="text-xs font-medium">Max output tokens<Input aria-label="Max output tokens" className="mt-1" type="number" min={1} max={131072} placeholder="Model default" value={session.generation.maxTokens ?? ""} onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 131072)) update({ generation: { ...session.generation, maxTokens: e.target.value ? n : undefined } }); }} /></label>
          </div>}
          {showSettings && session.mode === "router" && <div className="grid gap-3 border-b border-border bg-secondary/20 p-4 sm:grid-cols-4">
            <label className="text-xs font-medium">Tenant ID<Input aria-label="Tenant ID" className="mt-1" maxLength={200} placeholder="Optional" value={session.routerTenantId ?? ""} onChange={(e) => update({ routerTenantId: e.target.value })} /></label>
            <label className="text-xs font-medium">Effort<Select aria-label="Effort" className="mt-1 font-normal" value={session.routerEffort ?? ""} onValueChange={(value) => update({ routerEffort: value ? (value as "low" | "medium" | "high") : undefined })} options={[{ value: "", label: "Default" }, { value: "low", label: "Low" }, { value: "medium", label: "Medium" }, { value: "high", label: "High" }]} /></label>
            <label className="text-xs font-medium">Max output tokens<Input aria-label="Max output tokens" className="mt-1" type="number" min={1} max={131072} placeholder="Unset" value={session.routerMaxTokens ?? ""} onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 131072)) update({ routerMaxTokens: e.target.value ? n : undefined }); }} /></label>
            <div className="flex flex-col justify-end gap-1 text-xs">
              <label className="flex items-center gap-2"><input type="checkbox" checked={session.routerNeedsFunctionCalling ?? false} onChange={(e) => update({ routerNeedsFunctionCalling: e.target.checked })} className="h-3.5 w-3.5" />Function calling</label>
              <label className="flex items-center gap-2"><input type="checkbox" checked={session.routerNeedsToolChoice ?? false} onChange={(e) => update({ routerNeedsToolChoice: e.target.checked })} className="h-3.5 w-3.5" />Forced tool choice</label>
              <label className="flex items-center gap-2"><input type="checkbox" checked={session.routerNeedsResponseSchema ?? false} onChange={(e) => update({ routerNeedsResponseSchema: e.target.checked })} className="h-3.5 w-3.5" />Response schema</label>
            </div>
          </div>}
          {showSettings && session.mode === "image" && <div className="grid gap-3 border-b border-border bg-secondary/20 p-4 sm:grid-cols-[1fr_9rem_10rem]">
            <label className="text-xs font-medium">Negative prompt<textarea aria-label="Negative prompt" id="image-negative-prompt" value={session.imageNegativePrompt ?? ""} maxLength={4000} onChange={(e) => update({ imageNegativePrompt: e.target.value })} className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 text-sm" rows={2} placeholder="blurry, watermark" /></label>
            <label className="text-xs font-medium">Steps<Input aria-label="Steps" id="image-steps" className="mt-1" type="number" min={1} max={150} placeholder="Backend default" value={session.imageSteps ?? ""} onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 150)) update({ imageSteps: e.target.value ? n : undefined }); }} /></label>
            <label className="text-xs font-medium">Seed<Input aria-label="Seed" id="image-seed" className="mt-1" type="number" min={0} max={4_294_967_295} placeholder="Random" value={session.imageSeed ?? ""} onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 0 && n <= 4_294_967_295)) update({ imageSeed: e.target.value ? n : undefined }); }} /></label>
            <p className="text-[11px] text-muted-foreground sm:col-span-3">Negative prompt, steps, and seed are not part of the OpenAI images API. They are passed through to the backend, which may ignore them.</p>
          </div>}
          <div className="min-h-0 flex-1" key={`${session.id}:${session.mode}`}>
            {session.mode === "router"
              ? <RouterWorkspace session={session} update={update} />
              : session.mode === "image"
              ? <ImageWorkspace storageKey={`${root}:${session.id}:image`} session={session} update={update} models={models} loading={loading} />
              : <UnifiedWorkspace storageKey={`${root}:${session.id}:compare`} session={session} update={update} models={models} loading={loading} />}
          </div>
        </div>
      </div>
    </div>
  );
}
