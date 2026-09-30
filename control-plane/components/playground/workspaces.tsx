"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Activity, ArrowUp, Check, ChevronDown, Copy, Paperclip, Plus, RotateCcw, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { CharoPanel } from "@/components/charo/charo-panel";
import { useCharoStream, type ChatTurn } from "@/components/charo/use-charo-stream";
import { uuid } from "@/lib/uuid";
import type { ModelRoute } from "@/lib/obleth";
import { acceptsImages } from "@/lib/models-model";
import { formatDurationMs } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { PendingAction, PlaygroundSession } from "./playground";
import { conversationRows } from "./conversation-rows";
import { MAX_LANES, ModelPicker } from "./model-picker";
import { ModelAvatar, Pill, SectionLabel, SettingsPanel, SliderField, capabilities, formatCost, formatTokens, modelLabel, perMillion } from "./ui";

const noop = () => {};
type Stream = ReturnType<typeof useCharoStream>;

/** Per-answer numbers pulled off the last assistant turn of a lane. */
function turnStats(messages: ChatTurn[], model?: ModelRoute) {
  const last = [...messages].reverse().find((m) => m.role === "assistant");
  if (!last) return null;
  const m = last.metrics;
  const generating = m ? Math.max(0, m.totalMs - (m.ttftMs ?? 0)) : 0;
  const tps = m?.outputTokens && generating > 0 ? m.outputTokens / (generating / 1000) : undefined;
  // The trace carries the gateway's own accounting once it flushes; until
  // then (or when accounting is off) estimate from the model's list price.
  const traced = last.trace && last.trace.accountingAvailable !== false ? last.trace.costUsd : undefined;
  const estimated = model && m?.inputTokens !== undefined && m.outputTokens !== undefined
    ? m.inputTokens * model.input_cost_per_token + m.outputTokens * model.output_cost_per_token
    : undefined;
  return {
    streaming: !!last.streaming,
    error: !!last.error,
    ttftMs: m?.ttftMs,
    tps,
    inputTokens: m?.inputTokens,
    outputTokens: m?.outputTokens,
    cost: traced ?? estimated,
    costEstimated: traced === undefined && estimated !== undefined,
    requestId: last.requestId,
  };
}
type Stats = NonNullable<ReturnType<typeof turnStats>>;

function MetricsLine({ stats, highlight }: { stats: Stats; highlight?: Set<string> }) {
  const hl = (k: string) => (highlight?.has(k) ? "text-foreground" : undefined);
  return (
    <span className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11.5px] text-muted-foreground">
      {stats.ttftMs !== undefined && <span className={hl("ttft")}>{formatDurationMs(stats.ttftMs)} to first token</span>}
      {stats.tps !== undefined && <span>{Math.round(stats.tps)} tok/s</span>}
      {stats.outputTokens !== undefined && <span className={hl("tokens")}>{stats.inputTokens?.toLocaleString() ?? "—"} in · {stats.outputTokens.toLocaleString()} out</span>}
      {stats.cost !== undefined && <span className={hl("cost")} title={stats.costEstimated ? "Estimated from the model's list price" : "From the gateway's accounting"}>{stats.costEstimated ? "≈ " : ""}{formatCost(stats.cost)}</span>}
    </span>
  );
}

/** Labels for the best answer on each axis, when there is more than one answer to beat. */
function rowBadges(stats: Map<number, Stats | null>) {
  const done = [...stats.entries()].filter((e): e is [number, Stats] => !!e[1] && !e[1].streaming && !e[1].error);
  const out = new Map<number, { labels: string[]; keys: Set<string> }>();
  if (done.length < 2) return out;
  const best = (key: "ttftMs" | "outputTokens" | "cost", label: string, hl: string) => {
    const withValue = done.filter(([, s]) => s[key] !== undefined);
    if (withValue.length < 2) return;
    const [lane] = withValue.reduce((a, b) => ((b[1][key] as number) < (a[1][key] as number) ? b : a));
    const entry = out.get(lane) ?? { labels: [], keys: new Set<string>() };
    entry.labels.push(label); entry.keys.add(hl); out.set(lane, entry);
  };
  best("ttftMs", "Fastest first token", "ttft");
  best("outputTokens", "Fewest tokens", "tokens");
  best("cost", "Lowest cost", "cost");
  return out;
}

export function UnifiedWorkspace({ storageKey, session, update, models, loading, settingsOpen, onOpenSession, pending, onPendingDone, visionBoonActive = false }: {
  storageKey: string; session: PlaygroundSession; update: (patch: Partial<PlaygroundSession>) => void; models: ModelRoute[]; loading: boolean;
  settingsOpen: boolean;
  onOpenSession: (seed: Partial<PlaygroundSession>, action?: PendingAction) => void;
  pending?: PendingAction;
  onPendingDone: () => void;
  visionBoonActive?: boolean;
}) {
  // `supportsVision` decides whether an image -- the user's own, or a generated
  // one replayed -- goes to this slot's model as a real image part or has to
  // stay text; see `toWire`. A model opted into the vision boon takes images
  // while the boon is on, since the gateway describes them for it. Undefined
  // while the registry is still loading, which is the conservative reading.
  const options = (index: number) => {
    const name = session.models[index];
    return {
      storageKey: `${storageKey}:${index}`,
      model: name === "charo" ? undefined : name,
      supportsVision: acceptsImages(models.find((m) => m.model_name === name), visionBoonActive),
      generation: session.generation,
    };
  };
  // Stable slots retain independent histories and cancellation controllers.
  const first = useCharoStream(options(0));
  const second = useCharoStream(options(1));
  const third = useCharoStream(options(2));
  const fourth = useCharoStream(options(3));
  const all = [first, second, third, fourth];
  const streams = all.slice(0, session.models.length);
  const recipients = session.recipients?.filter((i) => i < streams.length) ?? streams.map((_, i) => i);
  const [text, setText] = useState("");
  const [image, setImage] = useState<string | undefined>();
  const [attachError, setAttachError] = useState<string | null>(null);
  const [picker, setPicker] = useState(false);
  const filePicker = useRef<HTMLInputElement>(null);
  const timeline = useRef<HTMLDivElement>(null);
  const busy = streams.some((s) => s.busy);
  const ready = !loading && streams.every((s) => s.ready) && recipients.length > 0 && recipients.every((i) => !!session.models[i]);
  const rows = conversationRows(streams.map((s) => s.messages));
  const lanes = session.models.map((name, i) => ({ name, i })).filter((l) => l.name);
  const multi = lanes.length > 1;
  const registry = (name: string) => models.find((m) => m.model_name === name);
  const locked = streams.length >= MAX_LANES && streams.every((s) => s.messages.length > 0);

  useEffect(() => {
    const el = timeline.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 160) el.scrollTo({ top: el.scrollHeight });
  });

  // A draft handed over from another mode (Router's "Open in Chat").
  useEffect(() => {
    if (session.chatDraft) { setText(session.chatDraft); update({ chatDraft: undefined }); }
    // Read once per mount; the patch clears it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const attachFile = (file?: File) => {
    if (!file) return;
    if (!file.type.startsWith("image/") || file.size > 6 * 1024 * 1024) { setAttachError("Choose an image up to 6 MB."); return; }
    const reader = new FileReader();
    reader.onload = () => { setImage(String(reader.result)); setAttachError(null); };
    reader.onerror = () => setAttachError("Could not read this image.");
    reader.readAsDataURL(file);
  };
  const send = (override?: string) => {
    const body = override ?? text;
    if (!ready || busy || (!body.trim() && !image)) return;
    const promptId = `2:${Date.now()}:${uuid()}`;
    recipients.forEach((i) => { void streams[i].send(body, image, undefined, promptId); });
    update(session.title === "Untitled session" ? { title: body.trim().slice(0, 60) || "Image conversation" } : {});
    setText(""); setImage(undefined);
    requestAnimationFrame(() => timeline.current?.scrollTo({ top: timeline.current.scrollHeight }));
  };
  const startActivity = (activity: string) => {
    const index = session.models.indexOf("charo");
    if (index >= 0) streams[index].startActivity(activity);
  };
  // Assistant tests run in an Assistant lane. From any other model, open one in
  // a fresh session rather than bolting it onto this conversation.
  const runAssistant = (activity: string) => {
    if (session.models.includes("charo") && recipients.includes(session.models.indexOf("charo"))) startActivity(activity);
    else onOpenSession({ title: activity === "benchmark" ? "Benchmark" : "Capability test", mode: "compare", models: ["charo"] }, { kind: "activity", value: activity });
  };

  // Work queued for this session by the shell (launcher quick start, or an
  // Assistant test opened from another session) runs once the lanes are ready.
  useEffect(() => {
    if (!pending || !ready || busy) return;
    if (pending.kind === "send") send(pending.value);
    else startActivity(pending.value);
    onPendingDone();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending, ready, busy]);

  /** Point the session at `names`, reusing lanes that already talk to them. */
  const choose = (names: string[]) => {
    const next = [...session.models];
    const chosen: number[] = [];
    for (const name of names) {
      let idx = next.indexOf(name);
      if (idx < 0) {
        idx = next.findIndex((n, i) => !n || (!chosen.includes(i) && !names.includes(n) && all[i].messages.length === 0));
        if (idx < 0) { if (next.length >= MAX_LANES) continue; idx = next.length; }
        else all[idx].selectTarget(null);
        next[idx] = name;
      }
      chosen.push(idx);
    }
    if (chosen.length) update({ models: next, recipients: chosen });
  };
  const toggleRecipient = (i: number) =>
    update({ recipients: recipients.includes(i) ? recipients.filter((r) => r !== i) : [...recipients, i].sort() });

  const primary = session.models[recipients[0] ?? 0] ?? "";
  const lastRow = rows.at(-1);

  return (
    <div className="relative flex h-full min-h-0">
      <section
        aria-label="Conversation"
        className="flex min-w-0 flex-1 flex-col"
        onDragOver={(e) => { if (Array.from(e.dataTransfer.types).includes("Files")) e.preventDefault(); }}
        onDrop={(e) => {
          if ((e.target as HTMLElement).closest("[data-charo-dropzone]")) return;
          if (e.dataTransfer.files.length) { e.preventDefault(); attachFile(e.dataTransfer.files[0]); }
        }}
      >
        {streams.map((s, i) => s.persistenceError && <p key={i} role="alert" className="px-6 py-2 text-xs text-muted-foreground">{modelLabel(session.models[i])}: {s.persistenceError}</p>)}
        <div ref={timeline} className="min-h-0 flex-1 overflow-y-auto px-4 py-6 md:px-8" aria-label="Conversation timeline">
          <div className={cn("mx-auto flex min-h-full flex-col gap-8", multi ? "max-w-6xl" : "max-w-3xl")}>
            {!rows.length && (
              <div className="m-auto flex max-w-lg flex-col items-center gap-3 py-10 text-center">
                {session.models.includes("charo") && recipients.includes(session.models.indexOf("charo")) ? (
                  <CharoPanel inline embedded open expanded stream={streams[session.models.indexOf("charo")]} onClose={noop} onExpand={noop} onCollapse={noop} />
                ) : (
                  <>
                    <h2 className="text-lg font-medium">{multi ? "One question, several answers." : "Ready when you are."}</h2>
                    <p className="text-sm text-muted-foreground">
                      {multi
                        ? "Your message goes to every model under Send to. Each keeps its own history."
                        : <>Chatting with <span className="text-foreground">{modelLabel(primary)}</span>. Add up to four models with Compare to see their answers side by side.</>}
                    </p>
                  </>
                )}
              </div>
            )}
            {rows.map((row) => {
              const entries = [...row.answers];
              const stats = new Map(entries.map(([i, msgs]) => [i, turnStats(msgs, registry(session.models[i]))]));
              const badges = rowBadges(stats);
              const asCards = entries.length > 1;
              return (
                <div key={row.id} className="flex flex-col gap-4">
                  {row.prompt && (
                    asCards ? (
                      <div className="flex flex-col items-center gap-1.5">
                        <span className="text-[11.5px] text-muted-foreground">You · sent to {entries.length} models</span>
                        <PromptBubble turn={row.prompt} centered />
                      </div>
                    ) : <div className="flex justify-end"><PromptBubble turn={row.prompt} /></div>
                  )}
                  <div className={cn("grid items-start gap-4", asCards && "md:grid-cols-2", entries.length === 3 && "xl:grid-cols-3", entries.length === 4 && "2xl:grid-cols-4")}>
                    {entries.map(([index, messages]) => (
                      <Answer
                        key={index}
                        card={asCards}
                        name={streams[index].activeTarget || session.models[index]}
                        model={registry(session.models[index])}
                        temperature={session.generation.temperature}
                        stream={streams[index]}
                        messages={messages}
                        stats={stats.get(index) ?? null}
                        badges={badges.get(index)}
                        latest={streams[index].messages.at(-1)?.id === messages.at(-1)?.id && row === lastRow}
                        canRetry={!!row.prompt}
                        onRemove={multi ? () => update({ recipients: recipients.filter((i) => i !== index) }) : undefined}
                        onContinue={streams.length > 1 ? () => update({ recipients: [index] }) : undefined}
                        continueDisabled={busy || (recipients.length === 1 && recipients[0] === index)}
                      />
                    ))}
                  </div>
                </div>
              );
            })}
          </div>
        </div>

        <div className="px-4 pb-4 md:px-8">
          <div className={cn("mx-auto flex flex-col gap-2", multi ? "max-w-4xl" : "max-w-3xl")}>
            <div className="rounded-2xl border border-border bg-card focus-within:border-muted-foreground/50">
              {multi && (
                <div className="flex flex-wrap items-center gap-1.5 px-3 pt-2.5">
                  <span className="mr-1 text-xs text-muted-foreground">Send to</span>
                  {lanes.map(({ name, i }) => {
                    const on = recipients.includes(i);
                    return (
                      <button key={i} type="button" aria-pressed={on} disabled={busy} onClick={() => toggleRecipient(i)}
                        className={cn("inline-flex h-[30px] items-center gap-1.5 rounded-lg border px-2.5 text-[12.5px] disabled:opacity-60", on ? "border-muted-foreground/40 bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                        <span className={cn("flex h-3.5 w-3.5 items-center justify-center rounded", on ? "bg-foreground text-background" : "border-[1.5px] border-muted-foreground/60")}>{on && <Check className="h-2.5 w-2.5" strokeWidth={3} />}</span>
                        {modelLabel(name)}
                      </button>
                    );
                  })}
                  <button type="button" onClick={() => setPicker(true)} disabled={busy || loading}
                    className="inline-flex h-[30px] items-center gap-1.5 rounded-lg border border-dashed border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60">
                    <Plus className="h-3.5 w-3.5" />Add model
                  </button>
                  <span className="ml-auto text-[11.5px] text-muted-foreground">{recipients.length} of {MAX_LANES}</span>
                </div>
              )}
              {image && (
                <div className="flex items-center gap-2 px-4 pt-3">
                  {/* eslint-disable-next-line @next/next/no-img-element */}
                  <img src={image} alt="Shared attachment" className="h-12 rounded" />
                  <Button variant="ghost" size="sm" onClick={() => setImage(undefined)}>Remove image</Button>
                </div>
              )}
              <textarea
                aria-label="Message"
                rows={2}
                className="block w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"
                value={text}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); } }}
                placeholder={!ready ? (recipients.length ? "Loading…" : "Choose who to send to…") : multi ? `Message ${recipients.length} model${recipients.length === 1 ? "" : "s"}…` : `Message ${modelLabel(primary)}…`}
              />
              <div className="flex items-center gap-1.5 px-2.5 pb-2.5 pt-2">
                {!multi && (
                  <>
                    <button type="button" onClick={() => setPicker(true)} disabled={busy || loading} aria-label={`Model: ${modelLabel(primary)}. Change model`}
                      className="inline-flex h-[30px] min-w-0 items-center gap-1.5 rounded-lg border border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60">
                      <ModelAvatar name={primary} size="xs" /><span className="truncate">{modelLabel(primary)}</span><ChevronDown className="h-3.5 w-3.5 shrink-0" />
                    </button>
                    <button type="button" onClick={() => setPicker(true)} disabled={busy || loading}
                      className="inline-flex h-[30px] items-center gap-1.5 rounded-lg border border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60">
                      <Plus className="h-3.5 w-3.5" />Compare
                    </button>
                  </>
                )}
                <input ref={filePicker} type="file" accept="image/*" className="hidden" onChange={(e) => { attachFile(e.target.files?.[0]); e.target.value = ""; }} />
                <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" title="Attach image" aria-label="Attach image" onClick={() => filePicker.current?.click()}><Paperclip className="h-4 w-4" /></Button>
                <div className="flex-1" />
                {busy
                  ? <Button variant="outline" className="h-9" onClick={() => streams.forEach((s) => s.stop())}><Square className="h-3 w-3" />{multi ? "Stop all" : "Stop"}</Button>
                  : <Button size="icon" className="h-9 w-9 rounded-[10px]" aria-label="Send" title="Send" disabled={!ready || (!text.trim() && !image)} onClick={() => send()}><ArrowUp className="h-4 w-4" /></Button>}
              </div>
            </div>
            {attachError && <p role="alert" className="text-xs text-destructive">{attachError}</p>}
            <p className="flex flex-wrap items-center justify-center gap-x-3 text-center text-xs text-muted-foreground">
              <span>Enter to send · Shift + Enter for a new line</span>
              {rows.length > 0 && <button type="button" onClick={() => streams.forEach((s) => s.reset())} disabled={busy} className="underline underline-offset-[3px] hover:text-foreground disabled:opacity-50">Clear conversation</button>}
            </p>
          </div>
        </div>
      </section>

      {settingsOpen && (
        <ChatSettings
          session={session}
          update={update}
          lanes={recipients.map((i) => session.models[i]).filter(Boolean)}
          registry={registry}
          onChangeModel={() => setPicker(true)}
          onAssistant={runAssistant}
          disabled={busy || loading}
        />
      )}
      <ModelPicker
        open={picker}
        onOpenChange={setPicker}
        models={models}
        selected={recipients.map((i) => session.models[i]).filter(Boolean)}
        locked={locked}
        onChoose={choose}
      />
    </div>
  );
}

function PromptBubble({ turn, centered }: { turn: ChatTurn; centered?: boolean }) {
  return (
    <div className={cn("max-w-2xl whitespace-pre-wrap break-words rounded-2xl border border-border bg-secondary/60 px-4 py-3 text-sm leading-relaxed", centered ? "text-center" : "rounded-br-md")}>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      {turn.image && <img src={turn.image} alt="Shared attachment" className="mb-2 max-h-40 rounded" />}
      {turn.content}
    </div>
  );
}

function Answer({ card, name, model, temperature, stream, messages, stats, badges, latest, canRetry, onRemove, onContinue, continueDisabled }: {
  card: boolean; name: string; model?: ModelRoute; temperature?: number; stream: Stream; messages: ChatTurn[]; stats: Stats | null;
  badges?: { labels: string[]; keys: Set<string> }; latest: boolean; canRetry: boolean;
  onRemove?: () => void; onContinue?: () => void; continueDisabled: boolean;
}) {
  const copy = () => void navigator.clipboard.writeText(messages.map((m) => m.content).join("\n\n")).catch(() => {});
  const label = modelLabel(name);
  const meta = [temperature !== undefined ? `temp ${temperature}` : null, model?.context_window ? formatTokens(model.context_window) : null].filter(Boolean).join(" · ");
  const actions = (
    <>
      <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" title="Copy answer" aria-label="Copy answer" onClick={copy}><Copy className="h-3.5 w-3.5" /></Button>
      {latest && (stream.busy
        ? <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" title="Stop this response" aria-label="Stop this response" onClick={stream.stop}><Square className="h-3.5 w-3.5" /></Button>
        : canRetry && <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" title="Retry this response" aria-label="Retry this response" disabled={!stream.ready} onClick={() => void stream.retry()}><RotateCcw className="h-3.5 w-3.5" /></Button>)}
    </>
  );
  const trace = stats?.requestId && (
    <Link href={`/logs?requestId=${encodeURIComponent(stats.requestId)}`} className="inline-flex items-center gap-1 text-xs text-muted-foreground underline-offset-[3px] hover:text-foreground hover:underline">
      <Activity className="h-3.5 w-3.5" />Trace
    </Link>
  );
  const body = <CharoPanel inline embedded hideComposer hideTrace variant="plain" open expanded stream={{ ...stream, messages }} onClose={noop} onExpand={noop} onCollapse={noop} />;

  if (!card) {
    return (
      <article aria-label={`${label} response`} className="flex min-w-0 flex-col gap-2.5">
        <div className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
          <ModelAvatar name={name} /><span className="font-medium text-foreground">{label}</span>{temperature !== undefined && <span>temp {temperature}</span>}
          {stats?.streaming && <Pill>Generating…</Pill>}
        </div>
        {body}
        {stats && !stats.streaming && (
          <div className="flex flex-wrap items-center gap-2">
            <MetricsLine stats={stats} />
            <div className="ml-auto flex items-center gap-1">{actions}{trace}</div>
          </div>
        )}
      </article>
    );
  }
  return (
    <article aria-label={`${label} response`} className="flex min-w-0 flex-col overflow-hidden rounded-2xl border border-border bg-card/60">
      <div className="flex items-center gap-2 border-b border-border py-2.5 pl-3.5 pr-2">
        <ModelAvatar name={name} />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-semibold" title={label}>{label}</div>
          {meta && <div className="text-[11.5px] text-muted-foreground">{meta}</div>}
        </div>
        {actions}
        {onRemove && <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" title={`Stop sending to ${label}`} aria-label={`Remove ${label}`} onClick={onRemove}><X className="h-3.5 w-3.5" /></Button>}
      </div>
      <div className="p-3.5">{body}</div>
      {(stats || onContinue) && (
        <div className="mt-auto space-y-2 border-t border-border px-3.5 py-2.5">
          {badges && <div className="flex flex-wrap gap-1.5">{badges.labels.map((b) => <Pill key={b}>{b}</Pill>)}</div>}
          {stats && !stats.streaming && <div className="flex flex-wrap items-center gap-2"><MetricsLine stats={stats} highlight={badges?.keys} /><span className="ml-auto">{trace}</span></div>}
          {stats?.streaming && <Pill>Generating…</Pill>}
          {onContinue && <Button variant="outline" size="sm" className="w-full" disabled={continueDisabled} onClick={onContinue}>Continue with this model</Button>}
        </div>
      )}
    </article>
  );
}

function ChatSettings({ session, update, lanes, registry, onChangeModel, onAssistant, disabled }: {
  session: PlaygroundSession; update: (patch: Partial<PlaygroundSession>) => void; lanes: string[];
  registry: (name: string) => ModelRoute | undefined; onChangeModel: () => void; onAssistant: (activity: string) => void; disabled: boolean;
}) {
  const g = session.generation;
  const setGen = (patch: Partial<typeof g>) => update({ generation: { ...g, ...patch } });
  const single = lanes.length <= 1;
  const primary = registry(lanes[0] ?? "");
  const noSystem = lanes.map(registry).filter((m) => m && !m.supports_system_messages).map((m) => m!.model_name);
  const ceiling = single && primary?.context_window ? primary.context_window : 131_072;

  return (
    <SettingsPanel
      title="Run settings"
      label="Run settings"
      action={<Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => update({ generation: { systemPrompt: "" } })}>Reset to defaults</Button>}
      footer={<p className="text-[11.5px] text-muted-foreground">Settings apply to the next message{single ? "" : " and to every model"}.</p>}
    >
      <section className="space-y-2.5">
        <SectionLabel>{single ? "Model" : `Models · ${lanes.length}`}</SectionLabel>
        {single ? (
          <div className="space-y-3 rounded-xl border border-border bg-background/40 p-3">
            <div className="flex items-center gap-2.5">
              <ModelAvatar name={lanes[0] ?? ""} size="md" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px] font-semibold">{modelLabel(lanes[0] ?? "")}</div>
                <div className="text-xs text-muted-foreground">
                  {primary ? `${primary.model_type === "image" ? "Image" : "Chat"}${primary.context_window ? ` · ${formatTokens(primary.context_window)} context` : ""}`
                    : lanes[0] === "auto" ? "The gateway picks per request" : lanes[0] === "charo" ? "Guided tests and benchmarks" : ""}
                </div>
              </div>
              <Button variant="ghost" size="icon" className="h-[30px] w-[30px] text-muted-foreground" aria-label="Change model" title="Change model" disabled={disabled} onClick={onChangeModel}><ChevronDown className="h-3.5 w-3.5" /></Button>
            </div>
            {primary && primary.model_type !== "image" && (
              <div className="flex flex-wrap gap-1.5">
                {capabilities(primary).map((c) => <Pill key={c.key} off={!c.on}>{c.on && <Check className="h-3 w-3" />}{c.on ? c.label : `No ${c.label.toLowerCase()}`}</Pill>)}
              </div>
            )}
            {primary && (
              <div className="grid grid-cols-2 gap-2 text-xs">
                {primary.model_type === "image"
                  ? <div><div className="text-muted-foreground">Price</div><div className="font-mono">{primary.cost_per_image ? `$${+primary.cost_per_image.toPrecision(3)} / image` : "free"}</div></div>
                  : <>
                    <div><div className="text-muted-foreground">Input</div><div className="font-mono">{perMillion(primary.input_cost_per_token)} / 1M</div></div>
                    <div><div className="text-muted-foreground">Output</div><div className="font-mono">{perMillion(primary.output_cost_per_token)} / 1M</div></div>
                  </>}
              </div>
            )}
            {lanes[0] === "auto" && <p className="text-xs text-muted-foreground">Open the Router tab to see which model a prompt would go to, and why.</p>}
            <div className="flex gap-1.5">
              <Button variant="outline" size="sm" className="flex-1" disabled={disabled} onClick={() => onAssistant("test_capabilities")}>Test capabilities</Button>
              <Button variant="outline" size="sm" className="flex-1" disabled={disabled} onClick={() => onAssistant("benchmark")}>Benchmark</Button>
            </div>
          </div>
        ) : (
          <div className="space-y-1.5">
            {lanes.map((name) => {
              const m = registry(name);
              return (
                <div key={name} className="flex items-center gap-2.5 rounded-lg border border-border px-2.5 py-2">
                  <ModelAvatar name={name} />
                  <span className="min-w-0 flex-1 truncate text-[13px]">{modelLabel(name)}</span>
                  {m?.context_window ? <span className="font-mono text-[11.5px] text-muted-foreground">{formatTokens(m.context_window)}</span> : null}
                </div>
              );
            })}
            <Button variant="outline" size="sm" className="w-full" disabled={disabled} onClick={onChangeModel}>Change models</Button>
          </div>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center justify-between">
          <SectionLabel htmlFor="playground-system-prompt">System prompt</SectionLabel>
          <span className="font-mono text-[11px] text-muted-foreground">{g.systemPrompt.length.toLocaleString()} / 32,000</span>
        </div>
        <textarea
          id="playground-system-prompt"
          aria-label="System prompt"
          value={g.systemPrompt}
          maxLength={32000}
          rows={4}
          onChange={(e) => setGen({ systemPrompt: e.target.value })}
          placeholder="Instructions every model sees first"
          className="w-full resize-y rounded-lg border border-border bg-background px-2.5 py-2 text-[13px] leading-relaxed outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"
        />
        {noSystem.length > 0 && <p className="text-[11.5px] text-muted-foreground">{noSystem.join(", ")} {noSystem.length === 1 ? "does" : "do"} not accept system messages.</p>}
      </section>

      <section className="space-y-4">
        <SectionLabel>Sampling</SectionLabel>
        <div className="space-y-1.5">
          <SliderField
            id="playground-temperature"
            ariaLabel="Temperature"
            label="Temperature"
            value={g.temperature ?? 1}
            display={g.temperature === undefined ? "Default" : g.temperature.toFixed(1)}
            min={0}
            max={2}
            step={0.1}
            onChange={(v) => setGen({ temperature: v })}
          />
          <div className="flex justify-between text-[11.5px] text-muted-foreground">
            <span>Precise</span>
            {g.temperature !== undefined && <button type="button" className="underline underline-offset-2 hover:text-foreground" onClick={() => setGen({ temperature: undefined })}>Use model default</button>}
            <span>Creative</span>
          </div>
        </div>
        <div className="space-y-1.5">
          <label htmlFor="playground-max-tokens" className="text-[13px]">Max output tokens</label>
          <Input
            id="playground-max-tokens"
            aria-label="Max output tokens"
            type="number"
            min={1}
            max={131072}
            placeholder="Model default"
            value={g.maxTokens ?? ""}
            onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 131072)) setGen({ maxTokens: e.target.value ? n : undefined }); }}
          />
          <p className="text-[11.5px] text-muted-foreground">Up to {Math.min(ceiling, 131_072).toLocaleString()}{single && primary?.context_window ? " for this model" : ""}</p>
        </div>
      </section>
    </SettingsPanel>
  );
}
