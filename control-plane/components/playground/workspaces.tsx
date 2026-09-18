"use client";

import { useEffect, useRef, useState } from "react";
import { Copy, Paperclip, Plus, RotateCcw, Send, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { CharoPanel } from "@/components/charo/charo-panel";
import { useCharoStream } from "@/components/charo/use-charo-stream";
import type { ModelRoute } from "@/lib/obleth";
import type { PlaygroundSession } from "./playground";
import { conversationRows } from "./conversation-rows";

const noop = () => {};
const modelName = (name: string) => name === "charo" ? "Assistant" : name || "Choose a model";

export function UnifiedWorkspace({ storageKey, session, update, models, loading }: {
  storageKey: string; session: PlaygroundSession; update: (patch: Partial<PlaygroundSession>) => void; models: ModelRoute[]; loading: boolean;
}) {
  // `supportsVision` decides whether a generated image can be replayed to this
  // slot's model as a real image part or has to stay a text placeholder — see
  // `toWire`. Undefined while the registry is still loading, which is the
  // conservative reading.
  const options = (index: number) => {
    const name = session.models[index];
    return {
      storageKey: `${storageKey}:${index}`,
      model: name === "charo" ? undefined : name,
      supportsVision: models.find((m) => m.model_name === name)?.supports_vision,
      generation: session.generation,
    };
  };
  // Stable slots retain independent histories and cancellation controllers.
  const first = useCharoStream(options(0));
  const second = useCharoStream(options(1));
  const third = useCharoStream(options(2));
  const fourth = useCharoStream(options(3));
  const streams = [first, second, third, fourth].slice(0, session.models.length);
  const recipients = session.recipients?.filter((i) => i < streams.length) ?? streams.map((_, i) => i);
  const [text, setText] = useState("");
  const [image, setImage] = useState<string | undefined>();
  const [attachError, setAttachError] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const timeline = useRef<HTMLDivElement>(null);
  const busy = streams.some((s) => s.busy);
  const ready = !loading && streams.every((s) => s.ready) && recipients.length > 0 && recipients.every((i) => !!session.models[i]);
  const rows = conversationRows(streams.map((s) => s.messages));
  useEffect(() => {
    const el = timeline.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 160) el.scrollTo({ top: el.scrollHeight });
  });
  const attachFile = (file?: File) => {
    if (!file) return;
    if (!file.type.startsWith("image/") || file.size > 6 * 1024 * 1024) { setAttachError("Choose an image up to 6 MB."); return; }
    const reader = new FileReader();
    reader.onload = () => { setImage(String(reader.result)); setAttachError(null); };
    reader.onerror = () => setAttachError("Could not read this image.");
    reader.readAsDataURL(file);
  };
  const send = () => {
    if (!ready || busy || (!text.trim() && !image)) return;
    const promptId = `2:${Date.now()}:${crypto.randomUUID()}`;
    recipients.forEach((i) => { void streams[i].send(text, image, undefined, promptId); });
    if (session.title === "Untitled session") update({ title: text.trim().slice(0, 60) || "Image conversation" });
    setText(""); setImage(undefined);
    requestAnimationFrame(() => timeline.current?.scrollTo({ top: timeline.current.scrollHeight }));
  };
  const startActivity = (activity: string) => {
    const index = session.models.indexOf("charo");
    if (index >= 0) streams[index].startActivity(activity);
  };
  return <div className="flex h-full min-h-0 flex-col" onDragOver={(e) => { if (Array.from(e.dataTransfer.types).includes("Files")) e.preventDefault(); }} onDrop={(e) => {
    if ((e.target as HTMLElement).closest("[data-charo-dropzone]")) return;
    if (e.dataTransfer.files.length) { e.preventDefault(); attachFile(e.dataTransfer.files[0]); }
  }}>
    <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-3">
      {recipients.filter((index) => session.models[index]).map((index) => <div key={index} className="flex max-w-full items-center gap-2 rounded-full border border-violet-500/25 bg-violet-500/10 py-1.5 pl-3 pr-1.5 text-sm">
        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-violet-400" />
        <span className="truncate">{modelName(session.models[index])}</span>
        <button type="button" aria-label={`Remove ${modelName(session.models[index])}`} disabled={busy} onClick={() => update({ recipients: recipients.filter((i) => i !== index) })} className="rounded-full p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"><X className="h-3 w-3" /></button>
      </div>)}
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="sm" disabled={busy || loading || !streams.every((s) => s.ready)}><Plus className="mr-1.5 h-3.5 w-3.5" />{recipients.some((i) => session.models[i]) ? "Add model" : "Choose models"}</Button></DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-h-[min(24rem,var(--radix-dropdown-menu-content-available-height))] w-72 max-w-[calc(100vw-2rem)] overflow-y-auto rounded-xl p-2 shadow-xl">
          <DropdownMenuLabel className="text-xs text-muted-foreground">Models · select up to four</DropdownMenuLabel>
          <DropdownMenuSeparator />
          {[{ name: "charo", label: "Assistant", detail: "Guided tools and benchmarks" }, { name: "auto", label: "Automatic routing", detail: "Let the gateway choose" }, ...models.filter((m) => ["chat", "image"].includes(m.model_type) && !["auto", "charo"].includes(m.model_name)).map((m) => ({ name: m.model_name, label: m.model_name, detail: m.model_type === "image" ? "Image model" : "Chat model" }))].map((model) => {
            const existing = session.models.indexOf(model.name);
            const selected = existing >= 0 && recipients.includes(existing);
            const available = session.models.findIndex((name, i) => !name || (!recipients.includes(i) && streams[i].messages.length === 0));
            const full = existing < 0 && available < 0 && streams.length >= 4;
            return <DropdownMenuItem key={model.name} disabled={selected || full} onSelect={() => {
              if (existing >= 0) { update({ recipients: [...recipients, existing] }); return; }
              const index = available >= 0 ? available : streams.length;
              if (index < streams.length) streams[index].selectTarget(null);
              const next = [...session.models]; next[index] = model.name;
              update({ models: next, recipients: [...recipients.filter((i) => i !== index && !!session.models[i]), index] });
            }} className="flex items-center justify-between gap-3 rounded-lg px-3 py-2.5">
              <span className="min-w-0"><span className="block truncate">{model.label}</span><span className="block text-xs text-muted-foreground">{model.detail}</span></span>
              {selected && <span className="text-[11px] text-muted-foreground">Added</span>}
            </DropdownMenuItem>;
          })}
          {streams.length >= 4 && <p className="px-3 py-2 text-xs text-muted-foreground">Start a new session to compare a different set of models.</p>}
        </DropdownMenuContent>
      </DropdownMenu>
      <div className="ml-auto flex gap-1">
        {session.models.includes("charo") && <><Button variant="ghost" size="sm" disabled={busy} onClick={() => startActivity("test_capabilities")}>Capabilities</Button><Button variant="ghost" size="sm" disabled={busy} onClick={() => startActivity("benchmark")}>Benchmark</Button></>}
        <Button variant="ghost" size="sm" onClick={() => streams.forEach((s) => s.reset())}>Clear</Button>
      </div>
    </div>
    {streams.map((s, i) => s.persistenceError && <p key={i} role="alert" className="px-4 py-2 text-xs text-amber-600">{modelName(session.models[i])}: {s.persistenceError}</p>)}
    <div ref={timeline} className="min-h-0 flex-1 space-y-8 overflow-y-auto p-4 md:p-6" aria-label="Conversation timeline">
      {!rows.length && <div className="mx-auto flex min-h-full max-w-lg flex-col items-center justify-center gap-3 text-center"><h2 className="text-lg font-medium">One question. A few perspectives.</h2><p className="text-sm text-muted-foreground">Choose one model to chat, or add up to four to compare answers here. Each model keeps its own context.</p>{session.models.includes("charo") && <CharoPanel inline embedded open expanded stream={streams[session.models.indexOf("charo")]} onClose={noop} onExpand={noop} onCollapse={noop} />}</div>}
      {rows.map((row) => <div key={row.id} className="space-y-3">
        {row.prompt && <div className="ml-auto max-w-3xl rounded-xl border border-violet-500/20 bg-violet-500/10 px-4 py-3"><p className="mb-1 text-xs font-medium text-muted-foreground">You</p>{row.prompt.image && <img src={row.prompt.image} alt="Shared attachment" className="mb-2 max-h-40 rounded" />}<p className="whitespace-pre-wrap break-words text-sm">{row.prompt.content}</p></div>}
        <div className={`grid items-start gap-3 ${row.answers.size > 1 ? "md:grid-cols-2" : "grid-cols-1"} ${row.answers.size === 3 ? "2xl:grid-cols-3" : row.answers.size === 4 ? "2xl:grid-cols-4" : ""}`}>
          {[...row.answers].map(([index, messages]) => {
            const stream = streams[index];
            const latest = stream.messages.at(-1)?.id === messages.at(-1)?.id;
            return <section key={index} aria-label={`${modelName(session.models[index])} response`} className="min-w-0 overflow-hidden rounded-xl border border-border bg-secondary/10">
              <div className="flex items-center justify-between gap-2 border-b border-border px-3 py-2"><span className="truncate text-xs font-semibold">{stream.activeTarget || modelName(session.models[index])}</span><div className="flex items-center gap-1">
                <Button variant="ghost" size="icon" title="Copy answer" onClick={() => void navigator.clipboard.writeText(messages.map((m) => m.content).join("\n\n")).catch(() => {})}><Copy className="h-3.5 w-3.5" /></Button>
                {latest && (stream.busy ? <Button variant="ghost" size="icon" title="Stop this response" onClick={stream.stop}><Square className="h-3.5 w-3.5" /></Button> : row.prompt && <Button variant="ghost" size="icon" title="Retry this response" disabled={!stream.ready} onClick={() => void stream.retry()}><RotateCcw className="h-3.5 w-3.5" /></Button>)}
              </div></div>
              <CharoPanel inline embedded hideComposer open expanded stream={{ ...stream, messages }} onClose={noop} onExpand={noop} onCollapse={noop} />
              {streams.length > 1 && <div className="border-t border-border px-3 py-1"><Button variant="ghost" size="sm" disabled={busy || (recipients.length === 1 && recipients[0] === index)} onClick={() => update({ recipients: [index] })}>Continue with this model</Button></div>}
            </section>;
          })}
        </div>
      </div>)}
    </div>
    <div className="space-y-2 border-t border-border bg-background px-4 py-3">
      {image && <div className="flex items-center gap-2">{/* eslint-disable-next-line @next/next/no-img-element */}<img src={image} alt="Shared attachment" className="h-12 rounded" /><Button variant="ghost" size="sm" onClick={() => setImage(undefined)}>Remove image</Button></div>}
      {attachError && <p role="alert" className="text-xs text-destructive">{attachError}</p>}
      <div className="flex items-end gap-2 rounded-xl border border-border bg-secondary/10 p-2 focus-within:ring-1 focus-within:ring-ring">
        <textarea aria-label="Shared prompt" rows={2} className="min-w-0 flex-1 resize-none bg-transparent p-2 text-sm outline-none" value={text} onChange={(e) => setText(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); } }} placeholder={ready ? "Message selected models…" : "Choose models above to begin…"} />
        <input ref={picker} type="file" accept="image/*" className="hidden" onChange={(e) => {
          attachFile(e.target.files?.[0]); e.target.value = "";
        }} />
        <Button variant="ghost" size="icon" title="Attach shared image" onClick={() => picker.current?.click()}><Paperclip className="h-4 w-4" /></Button>
        {busy ? <Button variant="secondary" onClick={() => streams.forEach((s) => s.stop())}><Square className="mr-2 h-3 w-3" />Stop all</Button> : <Button disabled={!ready || (!text.trim() && !image)} onClick={send}><Send className="mr-2 h-4 w-4" />Send</Button>}
      </div>
      <p className="text-[11px] text-muted-foreground">Follow-ups include each model’s own answers. Parameters apply to the next request.</p>
    </div>
  </div>;
}
