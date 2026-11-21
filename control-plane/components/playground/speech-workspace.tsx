"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { AudioLines, ChevronDown, Download, Loader2, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { uuid } from "@/lib/uuid";
import type { ModelRoute } from "@/lib/obleth";
import { StreamingPlayer } from "@/lib/playground/speech-player";
import { WavParseError, WavStreamParser, pcm16Bytes, wavBlob, type WavFormat } from "@/lib/playground/wav-stream";
import { cn, getJson } from "@/lib/utils";
import { SPEECH_TEXT_MAX_LENGTH, type PlaygroundSession } from "./playground";
import { ModelAvatar, Pill, RunError, SectionLabel, SettingsPanel } from "./ui";
import { formatNumber, money, perMillion } from "@/lib/format";
import { useQuery } from "@tanstack/react-query";

// Rows longer than this start clamped, with a "Show all" toggle.
const CLAMP_CHARS = 280;
const CLAMP_LINES = 4;

/** The request behind one row of the timeline. */
interface RunParams {
  text: string;
  model: string;
  voice?: string;
  instructions?: string;
}

interface Run extends RunParams {
  id: string;
  /** `waiting` until the first audio arrives; `playing` until it has all been heard. */
  status: "waiting" | "playing" | "done" | "stopped" | "error";
  /** Whether this browser could play it as it arrived (Web Audio). */
  live: boolean;
  /** From the request to the first decoded audio. */
  firstAudioMs?: number;
  /** From the request to the last byte; unset when stopped early. */
  generationMs?: number;
  audioSeconds: number;
  costUsd: number;
  requestId?: string;
  error?: string;
  /** Object URL of the finished (or stopped) audio, for replay and download. */
  audioUrl?: string;
  /** File extension for the download. */
  audioExt?: string;
  at: number;
}

type VoiceList = { state: "ready" | "unsupported" | "error"; voices: string[] };

/** The voices a speech model's own server lists; none when it cannot list them. */
async function readVoices(model: string, signal: AbortSignal): Promise<VoiceList> {
  const json = await getJson<{ voices?: unknown }>(`/api/live/playground/voices?model=${encodeURIComponent(model)}`, { signal });
  const voices = Array.isArray(json.voices) ? json.voices.filter((v): v is string => typeof v === "string") : [];
  return { state: voices.length ? "ready" : "unsupported", voices };
}

/** "0.42 s", "6.1 s", "3 min 12 s". */
export function formatSeconds(s: number): string {
  if (s < 1) return `${s.toFixed(2)} s`;
  if (s < 60) return `${s.toFixed(1)} s`;
  const whole = Math.round(s);
  return `${Math.floor(whole / 60)} min ${whole % 60} s`;
}

/** Audio seconds per second of waiting, in words. */
export function formatSpeed(ratio: number): string {
  if (ratio >= 1.05) return `${ratio.toFixed(1)}× faster than real time`;
  if (ratio > 0.95) return "about as fast as real time";
  return `${(1 / ratio).toFixed(1)}× slower than real time`;
}

function extensionFor(type: string): string {
  if (/wav/.test(type)) return "wav";
  if (/mpeg|mp3/.test(type)) return "mp3";
  if (/ogg|opus/.test(type)) return "ogg";
  if (/flac/.test(type)) return "flac";
  if (/aac|mp4/.test(type)) return "m4a";
  return "audio";
}

/**
 * Text-to-speech bench for `audio_speech` models. Drives /v1/audio/speech
 * through the gateway (as the reserved internal tenant, via
 * /api/live/playground/speech) and plays the audio while it is still being
 * made: the response is WAV sent as raw bytes, decoded piece by piece
 * (wav-stream.ts) and scheduled back to back with Web Audio
 * (speech-player.ts). A backend that does not stream sends the whole file at
 * the end, and it simply plays when it arrives.
 *
 * Same three parts as the Image mode: the timeline, the composer, and the
 * settings panel. Parameters persist on the session; the audio is memory-only
 * (a minute of WAV is megabytes, far past what localStorage can hold). Each
 * finished row keeps its WAV as an object URL for replay and download, revoked
 * on Clear and on unmount. The session export carries the run details, not
 * the audio.
 */
export function SpeechWorkspace({
  session,
  update,
  models,
  loading,
  storageKey,
  settingsOpen = true,
}: {
  session: PlaygroundSession;
  update: (patch: Partial<PlaygroundSession>) => void;
  models: ModelRoute[];
  loading: boolean;
  storageKey: string;
  settingsOpen?: boolean;
}) {
  const [runs, setRuns] = useState<Run[]>([]);
  const timeline = useRef<HTMLDivElement>(null);
  const active = useRef<{ id: string; abort: AbortController; player: StreamingPlayer | null } | null>(null);
  const urls = useRef(new Set<string>());
  const mounted = useRef(true);

  const speechModels = models.filter((m) => m.model_type === "audio_speech");
  const target = session.speechModel && speechModels.some((m) => m.model_name === session.speechModel)
    ? session.speechModel
    : speechModels[0]?.model_name ?? "";
  const text = session.speechText ?? "";
  const voice = session.speechVoice ?? "";
  const busy = runs.some((r) => r.status === "waiting" || r.status === "playing");
  const perChar = speechModels.find((m) => m.model_name === target)?.cost_per_character ?? 0;
  const chars = text.trim().length;

  // Voices come from the model's own server, when it can list them.
  const voiceQuery = useQuery({
    queryKey: ["speech-voices", target],
    queryFn: ({ signal }) => readVoices(target, signal),
    enabled: Boolean(target),
    staleTime: 5 * 60_000,
    retry: false,
  });
  const voices: VoiceList | null = voiceQuery.isError ? { state: "error", voices: [] } : voiceQuery.data ?? null;
  const voicesLoading = Boolean(target) && voiceQuery.isPending;
  const listed = voices?.state === "ready" ? voices.voices : null;
  const voiceOptions = listed
    ? [{ value: "", label: "Server default" }, ...(voice && !listed.includes(voice) ? [voice] : []).concat(listed).map((v) => ({ value: v, label: v }))]
    : [];

  // Results are memory-only, so the session export is the only way to keep a
  // record of them; hook into the same event the other workspaces use. The
  // details of each run go out, the audio does not.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<Record<string, unknown>>).detail;
      detail[storageKey] = runs.map((r) => ({
        text: r.text, model: r.model, voice: r.voice, instructions: r.instructions, status: r.status,
        firstAudioMs: r.firstAudioMs, generationMs: r.generationMs, audioSeconds: r.audioSeconds,
        costUsd: r.costUsd, requestId: r.requestId, error: r.error, at: r.at,
      }));
    };
    window.addEventListener("playground-export", handler);
    return () => window.removeEventListener("playground-export", handler);
  }, [runs, storageKey]);

  // Leaving the mode or the page stops the audio and frees what it held.
  useEffect(() => {
    mounted.current = true;
    const held = urls.current;
    return () => {
      mounted.current = false;
      const a = active.current;
      active.current = null;
      a?.player?.stop();
      a?.abort.abort();
      held.forEach((u) => URL.revokeObjectURL(u));
      held.clear();
    };
  }, []);

  // Follow the newest row, unless the user has scrolled up to an earlier one.
  useEffect(() => {
    const el = timeline.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 160) {
      el.scrollTo({ top: el.scrollHeight });
    }
  });

  const patch = (id: string, next: Partial<Run> | ((run: Run) => Partial<Run>)) =>
    setRuns((all) => all.map((r) => (r.id === id ? { ...r, ...(typeof next === "function" ? next(r) : next) } : r)));
  const release = (id: string) => {
    if (active.current?.id === id) active.current = null;
  };
  const objectUrl = (blob: Blob) => {
    if (!mounted.current || !blob.size) return undefined;
    const url = URL.createObjectURL(blob);
    urls.current.add(url);
    return url;
  };

  async function speak(source: RunParams) {
    if (active.current) return;
    const params: RunParams = { text: source.text, model: source.model, voice: source.voice, instructions: source.instructions };
    const id = `${Date.now()}-${uuid()}`;
    const abort = new AbortController();
    // Made inside the click (or key press): browsers only let audio start from a gesture.
    const player = StreamingPlayer.create(() => {
      release(id);
      patch(id, (r) => (r.status === "playing" ? { status: "done" } : {}));
    });
    active.current = { id, abort, player };
    const started = performance.now();
    const perCharacter = speechModels.find((m) => m.model_name === params.model)?.cost_per_character ?? 0;
    setRuns((all) => [...all, {
      ...params, id, status: "waiting", live: player !== null, audioSeconds: 0,
      costUsd: perCharacter * params.text.length, at: Date.now(),
    }]);

    const pcm: Uint8Array[] = [];
    let format: WavFormat | null = null;
    let contentType = "audio/wav";
    // Bytes are held only until the WAV header has parsed. If they turn out
    // not to be WAV at all (a backend that ignored the requested format), the
    // whole response is kept and decoded as a file once it is complete.
    let head: Uint8Array[] = [];
    let raw: Uint8Array[] | null = null;
    const replay = () => raw
      ? objectUrl(new Blob(raw as BlobPart[], { type: contentType }))
      : format && pcm.length ? objectUrl(wavBlob(pcm, format)) : undefined;

    try {
      const res = await fetch("/api/live/playground/speech", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: params.model,
          input: params.text,
          ...(params.voice ? { voice: params.voice } : {}),
          ...(params.instructions ? { instructions: params.instructions } : {}),
        }),
        signal: abort.signal,
      });
      contentType = res.headers.get("content-type") || "audio/wav";
      if (!res.ok || contentType.includes("json") || !res.body) {
        const json = (await res.json().catch(() => null)) as { error?: string } | null;
        throw new Error(json?.error ?? (res.ok ? "The model returned no audio." : `Speech failed (${res.status}).`));
      }
      const requestId = res.headers.get("x-obleth-request-id") ?? undefined;
      if (requestId) patch(id, { requestId });

      const reader = res.body.getReader();
      // Aborting the fetch also errors its body, but cancel the reader too so
      // a pending read always settles when Stop is pressed.
      abort.signal.addEventListener("abort", () => void reader.cancel().catch(() => {}), { once: true });
      const parser = new WavStreamParser();
      let received = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        received += value.length;
        if (raw) { raw.push(value); continue; }
        let samples: Float32Array;
        try {
          samples = parser.push(value);
        } catch (e) {
          if (!(e instanceof WavParseError) || parser.format) throw e;
          raw = [...head, value];
          head = [];
          continue;
        }
        if (!parser.format) { head.push(value); continue; }
        head = [];
        format = parser.format;
        if (!samples.length) continue;
        pcm.push(pcm16Bytes(samples));
        player?.push(samples, format);
        const audioSeconds = parser.frames / format.sampleRate;
        const firstAudioMs = performance.now() - started;
        patch(id, (r) => ({
          audioSeconds,
          ...(r.firstAudioMs === undefined ? { firstAudioMs } : {}),
          ...(r.status === "waiting" ? { status: "playing" as const } : {}),
        }));
      }
      if (abort.signal.aborted) throw new Error("Stopped.");
      if (!received) throw new Error("The model returned no audio.");
      const generationMs = performance.now() - started;

      let audioSeconds: number | undefined;
      if (raw) {
        const file = new Blob(raw as BlobPart[], { type: contentType });
        if (!file.size) throw new Error("The model returned no audio.");
        if (player) {
          audioSeconds = await player.playEncoded(await file.arrayBuffer()).catch(() => {
            throw new Error(`The model sent audio this page cannot play (${contentType}).`);
          });
        }
      } else {
        parser.end();
        if (!parser.frames) throw new Error("The model returned no audio.");
      }

      const audioUrl = replay();
      const playing = player !== null && !player.closedOrDone;
      patch(id, (r) => ({
        generationMs,
        audioUrl,
        audioExt: raw ? extensionFor(contentType) : "wav",
        ...(audioSeconds !== undefined ? { audioSeconds } : {}),
        ...(r.firstAudioMs === undefined ? { firstAudioMs: generationMs } : {}),
        ...(r.status === "stopped" ? {} : { status: playing ? "playing" as const : "done" as const }),
      }));
      if (playing) player.end();
      else release(id);
    } catch (e) {
      if (abort.signal.aborted) {
        // Stopped (or the page went away): keep whatever audio had arrived.
        patch(id, { audioUrl: replay(), audioExt: raw ? extensionFor(contentType) : "wav" });
        return;
      }
      player?.stop();
      release(id);
      patch(id, { status: "error", error: e instanceof Error ? e.message : String(e) });
    }
  }

  const stop = () => {
    const a = active.current;
    if (!a) return;
    active.current = null;
    a.player?.stop();
    a.abort.abort();
    patch(a.id, (r) => (r.status === "waiting" || r.status === "playing" ? { status: "stopped" } : {}));
  };

  // Snapshot the composer and settings at submit time, so Read again
  // reproduces the request that produced that row.
  const submit = () => {
    if (busy || !target || !text.trim()) return;
    update(session.title === "Untitled session" ? { title: text.trim().slice(0, 60) } : {});
    void speak({
      text: text.trim(),
      model: target,
      voice: voice.trim() || undefined,
      instructions: session.speechInstructions?.trim() || undefined,
    });
  };
  const reuse = (run: Run) => update({
    speechText: run.text, speechModel: run.model, speechVoice: run.voice, speechInstructions: run.instructions,
  });
  // Voices belong to a model; a new model starts on its server's default.
  const pickModel = (name: string) => { if (name !== target) update({ speechModel: name, speechVoice: undefined }); };
  const clear = () => {
    urls.current.forEach((u) => URL.revokeObjectURL(u));
    urls.current.clear();
    setRuns([]);
  };

  const chip = "inline-flex h-[30px] items-center gap-1.5 rounded-lg border border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60";
  const voiceHint = !target
    ? null
    : listed
    ? "Server default leaves the choice to the model's server."
    : voices?.state === "unsupported"
    ? "This model's server does not list its voices. Type a voice name, or leave it empty for its default."
    : voices?.state === "error"
    ? "The voice list could not be loaded. Type a voice name, or leave it empty for the default."
    : null;

  return (
    <div className="relative flex h-full min-h-0">
      <section aria-label="Readings" className="flex min-w-0 flex-1 flex-col">
        <div ref={timeline} className="min-h-0 flex-1 overflow-y-auto px-4 py-6 md:px-8" aria-label="Speech timeline">
          <div className="mx-auto flex min-h-full max-w-4xl flex-col gap-7">
            {!runs.length && (
              <div className="m-auto flex max-w-lg flex-col items-center gap-3 py-10 text-center">
                <h2 className="text-lg font-medium">Paste text to hear it read aloud.</h2>
                <p className="text-sm text-muted-foreground">
                  {!loading && speechModels.length === 0
                    ? <>No text-to-speech models are registered yet. Add one on the <Link href="/models" className="underline underline-offset-2 hover:text-foreground">Models</Link> page to start listening.</>
                    : "On models that stream, the audio starts playing as soon as the first words are ready. Each reading shows how quickly it was made and what it cost."}
                </p>
              </div>
            )}
            {runs.length > 0 && (
              <div className="-mb-3 flex justify-end">
                <Button variant="ghost" size="sm" className="text-muted-foreground" disabled={busy} onClick={clear}>Clear results</Button>
              </div>
            )}
            {runs.map((run) => {
              const settled = run.status === "done" || run.status === "stopped";
              return (
                <article key={run.id} aria-label={`${run.model} reading`} className="flex flex-col gap-2.5">
                  <div className="flex flex-wrap items-start gap-2">
                    <div className="min-w-0 flex-1 space-y-1">
                      <RunText text={run.text} />
                      <div className="flex flex-wrap gap-x-3 font-mono text-[11.5px] text-muted-foreground">
                        <span>{run.model}</span>
                        <span>{run.voice ? `voice ${run.voice}` : "default voice"}</span>
                      </div>
                      {run.instructions && <p className="text-[11.5px] text-muted-foreground">Style: {run.instructions}</p>}
                      <Metrics run={run} />
                    </div>
                    <StatusPill run={run} />
                    <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => reuse(run)}>Reuse text</Button>
                    <Button variant="ghost" size="sm" className="text-muted-foreground" title="Read this text again" disabled={busy} onClick={() => void speak(run)}>Read again</Button>
                    {settled && run.audioUrl && (
                      <Button asChild variant="ghost" size="icon" className="h-8 w-8 text-muted-foreground">
                        <a href={run.audioUrl} download={`speech-${run.id}.${run.audioExt ?? "wav"}`} title="Download audio" aria-label="Download audio"><Download className="h-3.5 w-3.5" /></a>
                      </Button>
                    )}
                  </div>
                  {run.status === "error" && <RunError message={run.error ?? "Speech failed."} />}
                  {settled && run.audioUrl && (
                    <audio controls preload="metadata" src={run.audioUrl} aria-label={`Replay ${run.model} reading`} className="h-10 w-full max-w-xl" />
                  )}
                </article>
              );
            })}
          </div>
        </div>

        <div className="px-4 pb-4 md:px-8">
          <div className="mx-auto max-w-3xl rounded-2xl border border-border bg-card focus-within:border-muted-foreground/50">
            <textarea
              id="speech-text"
              aria-label="Text to read aloud"
              rows={6}
              maxLength={SPEECH_TEXT_MAX_LENGTH}
              className="block max-h-[40vh] w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"
              value={text}
              onChange={(e) => update({ speechText: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  submit();
                }
              }}
              placeholder={!loading && speechModels.length === 0 ? "Register a text-to-speech model to begin…" : "Paste or type the text you want to hear…"}
            />
            <div className="flex flex-wrap items-center gap-1.5 px-2.5 pb-2.5 pt-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} disabled={loading || speechModels.length === 0} aria-label={`Speech model: ${target || "none"}`}>
                    <ModelAvatar name={target || "?"} size="xs" /><span className="max-w-40 truncate">{target || "No model"}</span><ChevronDown className="h-3.5 w-3.5" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {speechModels.map((m) => <DropdownMenuItem key={m.model_name} onSelect={() => pickModel(m.model_name)}>{m.model_name}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              {listed ? (
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <button type="button" className={chip} aria-label={`Voice: ${voice || "server default"}`}>
                      <span className="max-w-32 truncate">{voice || "Default voice"}</span><ChevronDown className="h-3.5 w-3.5" />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="start" className="max-h-72 overflow-y-auto">
                    {voiceOptions.map((o, i) => <DropdownMenuItem key={`${i}:${o.value}`} onSelect={() => update({ speechVoice: o.value || undefined })}>{o.label}</DropdownMenuItem>)}
                  </DropdownMenuContent>
                </DropdownMenu>
              ) : voicesLoading ? (
                <button type="button" className={chip} disabled aria-label="Voice: loading"><Loader2 className="h-3 w-3 animate-spin" />Voices</button>
              ) : (
                <input
                  id="speech-voice-chip"
                  aria-label="Voice name"
                  maxLength={200}
                  disabled={!target}
                  value={voice}
                  onChange={(e) => update({ speechVoice: e.target.value || undefined })}
                  placeholder="Default voice"
                  className={cn(chip, "w-32 bg-transparent outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring")}
                />
              )}
              <div className="flex-1" />
              <span className="font-mono text-[11.5px] tabular-nums text-muted-foreground" title="Characters, out of the most one reading can take">
                {formatNumber(text.length)} / {formatNumber(SPEECH_TEXT_MAX_LENGTH)}
              </span>
              {perChar > 0 && chars > 0 && <span className="font-mono text-[11.5px] text-muted-foreground" title="List price × characters">≈ {money(perChar * chars, { precise: true })}</span>}
              {busy ? (
                <Button id="speech-stop" variant="outline" className="h-9" onClick={stop}><Square className="h-3 w-3" />Stop</Button>
              ) : (
                <Button id="speech-read" className="h-9" disabled={!target || !text.trim()} onClick={submit}>
                  <AudioLines className="h-4 w-4" />Read aloud
                </Button>
              )}
            </div>
          </div>
          <p className="mx-auto mt-2 max-w-3xl text-center text-xs text-muted-foreground">
            Audio stays in this tab only. Download anything you want to keep. Ctrl + Enter reads aloud.
          </p>
        </div>
      </section>

      {settingsOpen && (
        <SettingsPanel
          title="Speech settings"
          label="Speech settings"
          action={<Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => update({ speechVoice: undefined, speechInstructions: undefined })}>Reset to defaults</Button>}
        >
          <section className="space-y-2.5">
            <SectionLabel htmlFor="speech-target">Model</SectionLabel>
            <Select
              id="speech-target"
              aria-label="Speech model"
              className="w-full"
              value={target}
              onValueChange={pickModel}
              disabled={loading || speechModels.length === 0}
              placeholder={speechModels.length === 0 ? "No speech models registered" : "Select a model"}
              searchPlaceholder="Filter models"
              options={speechModels.map((m) => ({ value: m.model_name, label: m.model_name }))}
            />
            {target && <p className="text-xs text-muted-foreground">{perChar > 0 ? `${perMillion(perChar)} per million characters` : "No price set"}</p>}
          </section>

          <section className="space-y-2.5">
            <SectionLabel htmlFor="speech-voice">Voice</SectionLabel>
            {listed ? (
              <Select
                id="speech-voice"
                aria-label="Voice"
                className="w-full"
                value={voice}
                onValueChange={(v) => update({ speechVoice: v || undefined })}
                searchPlaceholder="Filter voices"
                options={voiceOptions}
              />
            ) : voicesLoading ? (
              <Select id="speech-voice" aria-label="Voice" className="w-full" value="" disabled placeholder="Loading voices…" options={[]} />
            ) : (
              <Input
                id="speech-voice"
                aria-label="Voice"
                maxLength={200}
                disabled={!target}
                placeholder="Server default"
                value={voice}
                onChange={(e) => update({ speechVoice: e.target.value || undefined })}
              />
            )}
            {voiceHint && <p className="text-[11.5px] leading-snug text-muted-foreground">{voiceHint}</p>}
          </section>

          <section className="space-y-2.5">
            <SectionLabel htmlFor="speech-instructions">Style instructions</SectionLabel>
            <textarea
              id="speech-instructions"
              aria-label="Style instructions"
              rows={3}
              maxLength={2000}
              value={session.speechInstructions ?? ""}
              onChange={(e) => update({ speechInstructions: e.target.value || undefined })}
              placeholder="calm and slow"
              className="w-full resize-y rounded-lg border border-border bg-background px-2.5 py-2 text-[13px] outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"
            />
            <p className="text-[11.5px] leading-snug text-muted-foreground">Some models follow this, for example &ldquo;calm and slow&rdquo;. Others ignore it.</p>
          </section>
        </SettingsPanel>
      )}
    </div>
  );
}

/** The text of a row, clamped to a few lines when it is long. */
function RunText({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const long = text.length > CLAMP_CHARS || text.split("\n").length > CLAMP_LINES;
  return (
    <div className="space-y-0.5">
      <p className={cn("whitespace-pre-wrap break-words text-sm leading-relaxed", long && !open && "line-clamp-4")}>{text}</p>
      {long && (
        <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="text-[11.5px] text-muted-foreground hover:text-foreground hover:underline">
          {open ? "Show less" : "Show all"}
        </button>
      )}
    </div>
  );
}

function StatusPill({ run }: { run: Run }) {
  if (run.status === "waiting") return <Pill><Loader2 className="h-3 w-3 animate-spin" />Waiting for audio</Pill>;
  if (run.status === "playing") {
    return run.live
      ? <Pill><AudioLines className="h-3 w-3" />Playing</Pill>
      : <Pill><Loader2 className="h-3 w-3 animate-spin" />Receiving audio</Pill>;
  }
  if (run.status === "done") return <Pill>Done</Pill>;
  if (run.status === "stopped") return <Pill off>Stopped</Pill>;
  return <Pill inverted>Error</Pill>;
}

/** How quickly it was made, in plain words. */
function Metrics({ run }: { run: Run }) {
  const parts: string[] = [];
  if (run.firstAudioMs !== undefined) parts.push(`first audio in ${formatSeconds(run.firstAudioMs / 1000)}`);
  if (run.audioSeconds > 0) parts.push(`${formatSeconds(run.audioSeconds)} of audio`);
  if (run.generationMs !== undefined) parts.push(`made in ${formatSeconds(run.generationMs / 1000)}`);
  if (run.generationMs && run.audioSeconds > 0) parts.push(formatSpeed(run.audioSeconds / (run.generationMs / 1000)));
  if (run.costUsd > 0 && run.status !== "error") parts.push(money(run.costUsd, { precise: true }));
  if (!parts.length) return null;
  return (
    <p className="flex flex-wrap gap-x-3 text-[12px] text-muted-foreground" aria-label="Reading details">
      {parts.map((p) => <span key={p}>{p}</span>)}
    </p>
  );
}
