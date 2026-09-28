"use client";

import { useEffect, useRef, useState } from "react";
import { z } from "zod";
import { ArrowDownToLine, Code2, PanelLeft, PanelRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { uuid } from "@/lib/uuid";
import { generationSchema } from "@/lib/charo/chat-request";
import { useEnabledModels } from "@/components/charo/use-enabled-models";
import { UnifiedWorkspace } from "./workspaces";
import { RouterWorkspace } from "./router-workspace";
import { ImageWorkspace } from "./image-workspace";
import { VerdictsWorkspace } from "./verdicts-workspace";
import { NewSession } from "./new-session";
import { GetCode } from "./get-code";
import { SessionsRail } from "./sessions-rail";
import { migrateLegacySessions } from "./session-migration";
import { MODES } from "./ui";
import { PlaygroundSkeleton } from "@/components/page-skeletons";
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
  id: z.string(), title: z.string(), mode: z.enum(["chat", "compare", "router", "image", "verdicts"]),
  models: z.array(z.string()).min(1).max(4), generation: generationSchema,
  recipients: z.array(z.number().int().min(0).max(3)).optional(),
  // Last touched, for ordering and grouping the session rail.
  updatedAt: z.number().optional(),
  // A new session shows the "What do you want to try?" launcher until a mode is picked.
  launcher: z.boolean().optional(),
  // Text handed to the chat composer by another mode (Router's "Open in Chat");
  // read once when the chat workspace mounts, then cleared.
  chatDraft: z.string().max(ROUTER_PROMPT_MAX_LENGTH).optional(),
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
  // Verdicts mode's form draft: the state text, the target model, and the
  // question builder rows live on the session for the same reason the router
  // fields do — a mode toggle remounts the workspace. Results are memory-only
  // (they are cheap to re-run and would otherwise bloat localStorage).
  verdictModel: z.string().max(200).optional(),
  verdictState: z.string().max(100_000).optional(),
  verdictQuestions: z.array(z.object({
    id: z.string().max(64),
    type: z.enum(["boolean", "choice", "score"]),
    instructions: z.string().max(4000),
    options: z.array(z.object({ name: z.string().max(200), description: z.string().max(1000) })).max(26).optional(),
    levels: z.array(z.string().max(500)).max(10).optional(),
    trueDesc: z.string().max(1000).optional(),
    falseDesc: z.string().max(1000).optional(),
  })).max(32).optional(),
});
export type PlaygroundSession = z.infer<typeof sessionSchema>;

/** Work the shell queues for a session's chat workspace to run once it is ready. */
export type PendingAction = { kind: "send" | "activity"; value: string };

const fresh = (): PlaygroundSession => ({
  id: uuid(), title: "Untitled session", mode: "compare", models: ["auto"], generation: { systemPrompt: "" },
  launcher: true, updatedAt: Date.now(),
});

export function Playground({ scope, gatewayBase = "http://localhost:8080", openModel }: {
  scope: string;
  gatewayBase?: string;
  /** From `?model=`: open a new session on this model (the Models page's "Try in Playground"). */
  openModel?: { name: string; type: string };
}) {
  const root = `obleth-playground:${encodeURIComponent(scope)}`;
  const [sessions, setSessions] = useState<PlaygroundSession[]>([]);
  const [active, setActive] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [showSessions, setShowSessions] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(true);
  const [codeOpen, setCodeOpen] = useState(false);
  const [pending, setPending] = useState<(PendingAction & { sessionId: string }) | null>(null);
  const { models, loading, error, reload } = useEnabledModels();
  useEffect(() => {
    // Narrow screens start with the work surface unobstructed.
    if (window.innerWidth < 1024) setSettingsOpen(false);
    if (window.innerWidth < 768) setShowSessions(false);
  }, []);
  useEffect(() => {
    try {
      const parsed = z.array(sessionSchema).max(100).safeParse(JSON.parse(localStorage.getItem(root) ?? "[]"));
      const stored = parsed.success && parsed.data.length ? parsed.data : [fresh()];
      migrateLegacySessions(stored, root, localStorage);
      setSessions(stored); setActive(stored[0].id);
    } catch { const session = fresh(); setSessions([session]); setActive(session.id); setNotice("Browser storage is unavailable. Sessions will last only until you leave."); }
  }, [root]);
  useEffect(() => {
    if (!sessions.length) return;
    try { localStorage.setItem(root, JSON.stringify(sessions)); }
    catch { setNotice("Session settings could not be saved. Export important conversations before leaving."); }
  }, [root, sessions]);
  const opened = useRef(false);
  useEffect(() => {
    if (!openModel || opened.current || !sessions.length) return;
    opened.current = true;
    const image = openModel.type === "image";
    const next: PlaygroundSession = {
      ...fresh(),
      title: openModel.name,
      launcher: false,
      mode: image ? "image" : "chat",
      models: [openModel.name],
      ...(image ? { imageModel: openModel.name } : {}),
    };
    setSessions((all) => [next, ...all]);
    setActive(next.id);
    // Drop the parameter so a reload does not open the model a second time.
    window.history.replaceState(null, "", window.location.pathname);
  }, [openModel, sessions.length]);
  const session = sessions.find((s) => s.id === active);
  const update = (patch: Partial<PlaygroundSession>) =>
    setSessions((all) => all.map((s) => s.id === active ? { ...s, ...patch, updatedAt: Date.now() } : s));
  const create = () => {
    const next = fresh(); setSessions((all) => [next, ...all]); setActive(next.id);
  };
  const openSession = (seed: Partial<PlaygroundSession>, action?: PendingAction) => {
    const next = { ...fresh(), launcher: false, ...seed };
    setSessions((all) => [next, ...all]); setActive(next.id);
    if (action) setPending({ ...action, sessionId: next.id });
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
    } catch { setNotice("Only the open conversations could be exported; browser storage is unavailable."); }
    const live: Record<string, unknown> = {};
    window.dispatchEvent(new CustomEvent("playground-export", { detail: live }));
    Object.entries(live).filter(([k]) => k.startsWith(`${root}:${active}:`)).forEach(([k, value]) => { conversations[k.slice(root.length + active.length + 2)] = value; });
    const url = URL.createObjectURL(new Blob([JSON.stringify({ session, conversations }, null, 2)], { type: "application/json" }));
    const link = document.createElement("a"); link.href = url; link.download = "playground-session.json"; link.click(); URL.revokeObjectURL(url);
  };
  // The inverse of exportSession: a fresh id, so importing twice never collides.
  const importSession = async (file: File) => {
    try {
      const data = JSON.parse(await file.text()) as { session?: unknown; conversations?: Record<string, unknown> };
      const parsed = sessionSchema.safeParse(data.session);
      if (!parsed.success) { setNotice("That file is not a Playground session export."); return; }
      const next = { ...parsed.data, id: uuid(), updatedAt: Date.now() };
      for (const [key, value] of Object.entries(data.conversations ?? {})) {
        // Only conversation lanes; images are not persisted anyway.
        if (/^compare:\d$/.test(key) && value) localStorage.setItem(`${root}:${next.id}:${key}`, JSON.stringify(value));
      }
      setSessions((all) => [next, ...all]); setActive(next.id); setNotice(null);
    } catch { setNotice("Could not import that file."); }
  };
  if (!session) return <PlaygroundSkeleton />;
  const hasSettings = !session.launcher && session.mode !== "verdicts";
  const tabActive = (mode: PlaygroundSession["mode"]) => !session.launcher && (session.mode === mode || (mode === "compare" && session.mode === "chat"));
  const sessionPending = pending?.sessionId === session.id ? pending : undefined;
  return (
    <div className="flex h-full min-h-[36rem] flex-col md:flex-row">
      {showSessions && (
        <SessionsRail
          sessions={sessions}
          active={active}
          onSelect={setActive}
          onCreate={create}
          onRemove={remove}
          onImport={(f) => void importSession(f)}
          canCreate={sessions.length < 50}
        />
      )}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {/* One control row for the whole page. The app shell already names the page and already owns the navigation toggle. */}
        <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border px-2 md:px-3">
          <div className="flex min-w-0 flex-1 items-center gap-1">
            <Button variant="ghost" size="icon" className="text-muted-foreground" title="Toggle session list" aria-label="Toggle session list" aria-expanded={showSessions} onClick={() => setShowSessions(!showSessions)}><PanelLeft className="h-4 w-4" /></Button>
            <input
              aria-label="Session name"
              maxLength={100}
              className="h-8 w-full min-w-0 max-w-xs truncate rounded-md bg-transparent px-2 text-sm font-semibold outline-none hover:bg-accent/50 focus:bg-accent/50 focus:ring-1 focus:ring-ring"
              value={session.title}
              onChange={(e) => update({ title: e.target.value })}
            />
          </div>
          <div role="group" aria-label="Playground mode" className="flex shrink-0 self-stretch">
            {MODES.map(({ mode, label, icon: Icon }) => (
              <button
                key={mode}
                type="button"
                aria-pressed={tabActive(mode)}
                title={label}
                aria-label={label}
                onClick={() => update({ mode, launcher: false })}
                className={cn(
                  "-mb-px flex items-center gap-1.5 border-b-2 px-2.5 text-[13px] font-medium transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring sm:px-3.5",
                  tabActive(mode) ? "border-violet-400 text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
                )}
              >
                <Icon className="h-3.5 w-3.5" aria-hidden="true" /><span className="hidden sm:inline">{label}</span>
              </button>
            ))}
          </div>
          <div className="flex flex-1 items-center justify-end gap-1">
            {!session.launcher && <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => setCodeOpen(true)}><Code2 className="h-4 w-4" /><span className="hidden lg:inline">Get code</span></Button>}
            <Button variant="ghost" size="icon" className="text-muted-foreground" title="Export session" aria-label="Export session" onClick={exportSession}><ArrowDownToLine className="h-4 w-4" /></Button>
            {hasSettings && (
              <Button variant="ghost" size="icon" className={cn("text-muted-foreground", settingsOpen && "bg-secondary text-foreground")} title={settingsOpen ? "Hide settings" : "Show settings"} aria-label={settingsOpen ? "Hide settings" : "Show settings"} aria-pressed={settingsOpen} onClick={() => setSettingsOpen(!settingsOpen)}>
                <PanelRight className="h-4 w-4" />
              </Button>
            )}
          </div>
        </header>
        {notice && <p role="alert" className="flex items-center justify-between gap-3 border-b border-border bg-secondary/40 px-4 py-2 text-xs text-secondary-foreground">{notice}<button type="button" className="underline" onClick={() => setNotice(null)}>Dismiss</button></p>}
        {error && <div role="alert" className="flex items-center gap-3 border-b border-border bg-secondary/40 px-4 py-2 text-sm">{error}<Button variant="outline" size="sm" onClick={reload}>Retry loading models</Button></div>}
        <div className="relative min-h-0 flex-1" key={`${session.id}:${session.launcher ? "launcher" : session.mode}`}>
          {session.launcher
            ? <NewSession
                onPick={(mode) => update({ mode, launcher: false })}
                onQuickStart={(text) => { update({ mode: "compare", models: ["auto"], recipients: [0], launcher: false }); setPending({ sessionId: session.id, kind: "send", value: text }); }}
                onAssistant={(activity) => { update({ mode: "compare", models: ["charo"], recipients: [0], launcher: false, title: activity === "benchmark" ? "Benchmark" : "Capability test" }); setPending({ sessionId: session.id, kind: "activity", value: activity }); }}
              />
            : session.mode === "router"
            ? <RouterWorkspace session={session} update={update} settingsOpen={settingsOpen} onOpenSession={openSession} />
            : session.mode === "image"
            ? <ImageWorkspace storageKey={`${root}:${session.id}:image`} session={session} update={update} models={models} loading={loading} settingsOpen={settingsOpen} />
            : session.mode === "verdicts"
            ? <VerdictsWorkspace session={session} update={update} models={models} loading={loading} />
            : <UnifiedWorkspace storageKey={`${root}:${session.id}:compare`} session={session} update={update} models={models} loading={loading} settingsOpen={settingsOpen} onOpenSession={openSession} pending={sessionPending} onPendingDone={() => setPending(null)} />}
        </div>
      </div>
      <GetCode open={codeOpen} onOpenChange={setCodeOpen} session={session} gatewayBase={gatewayBase} />
    </div>
  );
}
