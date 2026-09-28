"use client";

import { useEffect, useRef, useState } from "react";
import { ChevronDown, Download, Expand, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { uuid } from "@/lib/uuid";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";
import { ModelAvatar, SectionLabel, Segmented, SettingsPanel, formatCost } from "./ui";

// Sizes offered in the picker. Deliberately a superset of the boon's default
// allowed list: the playground drives the image model directly, so it is not
// bound by the boon's schema, and testing a size the boon does not offer is a
// legitimate thing to want to do.
const SIZES = ["256x256", "512x512", "768x768", "1024x1024"] as const;
const COUNTS = [1, 2, 3, 4] as const;

// Longer errors collapse behind a disclosure. Backends routinely return a
// whole framework traceback on one line (`litellm.InternalServerError: ...
// synStatus 31 ...`), which is worth keeping but not worth handing the full
// width of the timeline to.
const ERROR_LEAD_LENGTH = 120;

interface GeneratedImage {
  id: string;
  url: string;
  model: string;
  prompt: string;
  size: string;
  seed?: number;
  steps?: number;
  latencyMs: number;
  costUsd: number;
  at: number;
}

/** The request behind one row of the timeline. */
interface RunParams {
  prompt: string;
  model: string;
  size: string;
  count: number;
  negativePrompt?: string;
  steps?: number;
  seed?: number;
}

interface Run extends RunParams {
  id: string;
  status: "busy" | "done" | "error";
  images: GeneratedImage[];
  error?: string;
}

/**
 * Parameterised test bench for `image`-type models. Drives
 * /v1/images/generations through the gateway rather than the chat relay, so
 * size, count, steps, and seed are actually settable.
 *
 * Same three parts as chat: the gallery, the composer (prompt plus quick
 * pickers for what changes constantly), and the settings panel, which holds
 * every parameter including the backend-only ones.
 *
 * The gallery is intentionally in-memory: a 1024px PNG is 1–2 MB as base64 and
 * `playground.tsx` persists sessions to localStorage (~5 MB), so a persisted
 * gallery would exhaust the quota after two or three results and start failing
 * saves of the session list. Parameters persist; images are downloadable and
 * ride the existing `playground-export` event under `storageKey`, the same
 * convention `use-charo-stream.ts` follows for its own exported state.
 */
export function ImageWorkspace({
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

  const imageModels = models.filter((m) => m.model_type === "image");
  const target = session.imageModel ?? imageModels[0]?.model_name ?? "";
  const prompt = session.imagePrompt ?? "";
  const size = session.imageSize ?? "512x512";
  const count = session.imageCount ?? 1;
  const busy = runs.some((r) => r.status === "busy");
  const perImage = imageModels.find((m) => m.model_name === target)?.cost_per_image ?? 0;

  // Results are memory-only, so the session export is the only way to keep
  // them; hook into the same event the chat workspaces use. Flattened to the
  // images themselves — a run that failed produced nothing to export.
  const images = runs.flatMap((r) => r.images);
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<Record<string, unknown>>).detail;
      detail[storageKey] = images;
    };
    window.addEventListener("playground-export", handler);
    return () => window.removeEventListener("playground-export", handler);
    // `images` is derived from `runs`, so this is exact, not a stale closure.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runs, storageKey]);

  // Follow the newest row the way the chat timeline does, but leave the view
  // alone if the user has scrolled up to look at an earlier result.
  useEffect(() => {
    const el = timeline.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 160) {
      el.scrollTo({ top: el.scrollHeight });
    }
  });

  const patch = (id: string, next: Partial<Run>) =>
    setRuns((all) => all.map((r) => (r.id === id ? { ...r, ...next } : r)));

  async function generate(params: RunParams) {
    const id = `${Date.now()}-${uuid()}`;
    setRuns((all) => [...all, { ...params, id, status: "busy", images: [] }]);
    try {
      const res = await fetch("/api/live/playground/images", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: params.model,
          prompt: params.prompt,
          size: params.size,
          n: params.count,
          ...(params.negativePrompt ? { negative_prompt: params.negativePrompt } : {}),
          ...(params.steps !== undefined ? { steps: params.steps } : {}),
          ...(params.seed !== undefined ? { seed: params.seed } : {}),
        }),
      });
      const json = (await res.json()) as
        | { images: string[]; latencyMs: number; requestId: string | null }
        | { error: string };
      if (!res.ok || "error" in json) {
        patch(id, {
          status: "error",
          error: "error" in json ? json.error : `Generation failed (${res.status}).`,
        });
        return;
      }
      const cost = imageModels.find((m) => m.model_name === params.model)?.cost_per_image ?? 0;
      const at = Date.now();
      patch(id, {
        status: "done",
        images: json.images.map((url, i) => ({
          id: `${id}-${i}`,
          url,
          model: params.model,
          prompt: params.prompt,
          size: params.size,
          seed: params.seed,
          steps: params.steps,
          latencyMs: json.latencyMs,
          costUsd: cost,
          at,
        })),
      });
    } catch (e) {
      patch(id, { status: "error", error: String(e) });
    }
  }

  // Snapshot the composer and settings at submit time. A run keeps its own
  // copy so Run again reproduces the request that produced that row, not
  // whatever the controls happen to say later.
  const submit = () => {
    if (busy || !target || !prompt.trim()) return;
    update(session.title === "Untitled session" ? { title: prompt.trim().slice(0, 60) } : {});
    void generate({
      prompt: prompt.trim(),
      model: target,
      size,
      count,
      negativePrompt: session.imageNegativePrompt?.trim() || undefined,
      steps: session.imageSteps,
      seed: session.imageSeed,
    });
  };
  const reuse = (run: Run) => update({
    imagePrompt: run.prompt, imageModel: run.model, imageSize: run.size, imageCount: run.count,
    imageNegativePrompt: run.negativePrompt, imageSteps: run.steps, imageSeed: run.seed,
  });
  const downloadAll = (run: Run) => run.images.forEach((image) => {
    const a = document.createElement("a"); a.href = image.url; a.download = `playground-${image.id}.png`; a.click();
  });

  const chip = "inline-flex h-[30px] items-center gap-1.5 rounded-lg border border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60";

  return (
    <div className="relative flex h-full min-h-0">
      <section aria-label="Generations" className="flex min-w-0 flex-1 flex-col">
        <div ref={timeline} className="min-h-0 flex-1 overflow-y-auto px-4 py-6 md:px-8" aria-label="Generation timeline">
          <div className="mx-auto flex min-h-full max-w-4xl flex-col gap-7">
            {!runs.length && (
              <div className="m-auto flex max-w-lg flex-col items-center gap-3 py-10 text-center">
                <h2 className="text-lg font-medium">Describe a picture.</h2>
                <p className="text-sm text-muted-foreground">
                  {imageModels.length === 0
                    ? "No image models are registered yet. Add one on the Models page to start generating."
                    : "Generations go straight through the gateway, so size, count, steps and seed all apply. Each result shows its latency and cost."}
                </p>
              </div>
            )}
            {runs.length > 0 && (
              <div className="-mb-3 flex justify-end">
                <Button variant="ghost" size="sm" className="text-muted-foreground" disabled={busy} onClick={() => setRuns([])}>Clear results</Button>
              </div>
            )}
            {runs.map((run) => (
              <article key={run.id} aria-label={`${run.model} result`} className="flex flex-col gap-2.5">
                <div className="flex flex-wrap items-start gap-2">
                  <div className="min-w-0 flex-1 space-y-1">
                    <p className="whitespace-pre-wrap break-words text-sm leading-relaxed">{run.prompt}</p>
                    <div className="flex flex-wrap gap-x-3 font-mono text-[11.5px] text-muted-foreground">
                      <span>{run.model}</span>
                      <span>{run.size.replace("x", "×")}</span>
                      <span>{run.count === 1 ? "1 image" : `${run.count} images`}</span>
                      <span>seed {run.seed ?? "random"}</span>
                      {run.steps !== undefined && <span>{run.steps} steps</span>}
                      {run.status === "done" && <span>{run.images[0]?.latencyMs} ms</span>}
                      {run.status === "done" && <span>${run.images.reduce((total, i) => total + i.costUsd, 0).toFixed(4)}</span>}
                    </div>
                    {run.negativePrompt && <p className="text-[11.5px] text-muted-foreground">Negative: {run.negativePrompt}</p>}
                  </div>
                  {run.status === "busy" && <span className="inline-flex h-[22px] items-center gap-1.5 rounded-full border border-border px-2 text-[11.5px]"><Loader2 className="h-3 w-3 animate-spin" />Generating</span>}
                  <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => reuse(run)}>Reuse prompt</Button>
                  <Button variant="ghost" size="sm" className="text-muted-foreground" title="Generate this prompt again" disabled={busy} onClick={() => void generate(run)}>Run again</Button>
                  {run.status === "done" && (
                    <Button variant="ghost" size="icon" className="h-8 w-8 text-muted-foreground" title="Download all" aria-label="Download all" onClick={() => downloadAll(run)}><Download className="h-3.5 w-3.5" /></Button>
                  )}
                </div>
                {run.status === "error" && <RunError message={run.error ?? "Generation failed."} />}
                {run.status !== "error" && (
                  <div className="grid max-w-3xl grid-cols-2 gap-2.5 sm:grid-cols-4">
                    {run.status === "busy" && Array.from({ length: run.count }, (_, i) => (
                      <div key={i} className="flex aspect-square items-center justify-center rounded-xl border border-dashed border-border bg-card text-[11.5px] text-muted-foreground">Generating…</div>
                    ))}
                    {run.images.map((image) => (
                      <figure key={image.id} className="group relative aspect-square overflow-hidden rounded-xl border border-border bg-card">
                        {/* eslint-disable-next-line @next/next/no-img-element */}
                        <img src={image.url} alt={image.prompt} className="h-full w-full object-cover" />
                        <figcaption className="absolute inset-x-1.5 top-1.5 flex justify-end gap-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
                          <a href={image.url} target="_blank" rel="noreferrer" aria-label="Open full size" title="Open full size" className="inline-flex h-[30px] w-[30px] items-center justify-center rounded-lg bg-background/80 text-foreground"><Expand className="h-3.5 w-3.5" /></a>
                          <a href={image.url} download={`playground-${image.id}.png`} aria-label="Download" title="Download" className="inline-flex h-[30px] w-[30px] items-center justify-center rounded-lg bg-background/80 text-foreground"><Download className="h-3.5 w-3.5" /></a>
                        </figcaption>
                        {image.seed !== undefined && (
                          <button type="button" onClick={() => update({ imageSeed: image.seed })} className="absolute bottom-1.5 left-1.5 h-[26px] rounded-md bg-background/80 px-2 text-[11.5px] opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100">Use this seed</button>
                        )}
                      </figure>
                    ))}
                  </div>
                )}
              </article>
            ))}
          </div>
        </div>

        <div className="px-4 pb-4 md:px-8">
          <div className="mx-auto max-w-3xl rounded-2xl border border-border bg-card focus-within:border-muted-foreground/50">
            <textarea
              id="image-prompt"
              aria-label="Prompt"
              rows={2}
              maxLength={4000}
              className="block w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-sm leading-relaxed outline-none placeholder:text-muted-foreground"
              value={prompt}
              onChange={(e) => update({ imagePrompt: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  submit();
                }
              }}
              placeholder={imageModels.length === 0 ? "Register an image model to begin…" : "Describe the image you want…"}
            />
            <div className="flex flex-wrap items-center gap-1.5 px-2.5 pb-2.5 pt-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} disabled={loading || imageModels.length === 0} aria-label={`Image model: ${target || "none"}`}>
                    <ModelAvatar name={target || "?"} size="xs" /><span className="max-w-40 truncate">{target || "No model"}</span><ChevronDown className="h-3.5 w-3.5" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {imageModels.map((m) => <DropdownMenuItem key={m.model_name} onSelect={() => update({ imageModel: m.model_name })}>{m.model_name}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} aria-label={`Size: ${size}`}>{size.replace("x", "×")}<ChevronDown className="h-3.5 w-3.5" /></button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {SIZES.map((s) => <DropdownMenuItem key={s} onSelect={() => update({ imageSize: s })}>{s.replace("x", "×")}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} aria-label={`Images per run: ${count}`}>{count === 1 ? "1 image" : `${count} images`}<ChevronDown className="h-3.5 w-3.5" /></button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {COUNTS.map((n) => <DropdownMenuItem key={n} onSelect={() => update({ imageCount: n })}>{n === 1 ? "1 image" : `${n} images`}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              <div className="flex-1" />
              {perImage > 0 && <span className="font-mono text-[11.5px] text-muted-foreground" title="List price × images">≈ {formatCost(perImage * count)}</span>}
              <Button id="image-generate" className="h-9" disabled={busy || !target || !prompt.trim()} onClick={submit}>
                {busy && <Loader2 className="h-4 w-4 animate-spin" />}
                Generate
              </Button>
            </div>
          </div>
          <p className="mx-auto mt-2 max-w-3xl text-center text-xs text-muted-foreground">
            Results stay in this tab only. Download anything you want to keep, or export the session.
          </p>
        </div>
      </section>

      {settingsOpen && (
        <SettingsPanel
          title="Image settings"
          label="Image settings"
          action={<Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => update({ imageSize: undefined, imageCount: undefined, imageNegativePrompt: undefined, imageSteps: undefined, imageSeed: undefined })}>Reset to defaults</Button>}
        >
          <section className="space-y-2.5">
            <SectionLabel htmlFor="image-target">Model</SectionLabel>
            <Select
              id="image-target"
              aria-label="Image model"
              className="w-full"
              value={target}
              onValueChange={(v) => update({ imageModel: v })}
              disabled={loading || imageModels.length === 0}
              placeholder={imageModels.length === 0 ? "No image models registered" : "Select a model"}
              searchPlaceholder="Filter models"
              options={imageModels.map((m) => ({ value: m.model_name, label: m.model_name }))}
            />
            {target && <p className="text-xs text-muted-foreground">{perImage > 0 ? `$${+perImage.toPrecision(3)} per image` : "No price set"}</p>}
          </section>

          <section className="space-y-2.5">
            <SectionLabel>Size</SectionLabel>
            <div role="group" aria-label="Size" className="grid grid-cols-4 gap-1.5">
              {SIZES.map((s, i) => (
                <button key={s} type="button" aria-pressed={size === s} onClick={() => update({ imageSize: s })}
                  className={cn("flex h-[72px] flex-col items-center justify-center gap-1.5 rounded-lg border text-[11.5px] transition-colors", size === s ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                  <span aria-hidden="true" className="rounded-sm border-[1.5px] border-current" style={{ width: 10 + i * 4, height: 10 + i * 4 }} />
                  {s.split("x")[0]}
                </button>
              ))}
            </div>
          </section>

          <section className="space-y-2.5">
            <SectionLabel>Images per run</SectionLabel>
            <Segmented label="Images per run" value={String(count)} onChange={(v) => update({ imageCount: Number(v) })} options={COUNTS.map((n) => ({ value: String(n), label: String(n) }))} />
          </section>

          <section className="space-y-3 border-t border-border pt-4">
            <div className="space-y-1">
              <SectionLabel>Backend options</SectionLabel>
              <p className="text-[11.5px] leading-snug text-muted-foreground">Negative prompt, steps and seed are not part of the OpenAI images API. They are passed through to the backend, which may ignore them.</p>
            </div>
            <div className="space-y-1.5">
              <label htmlFor="image-negative-prompt" className="text-[13px]">Negative prompt</label>
              <textarea aria-label="Negative prompt" id="image-negative-prompt" value={session.imageNegativePrompt ?? ""} maxLength={4000} rows={2}
                onChange={(e) => update({ imageNegativePrompt: e.target.value })}
                className="w-full resize-y rounded-lg border border-border bg-background px-2.5 py-2 text-[13px] outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring" placeholder="blurry, watermark" />
            </div>
            <div className="space-y-1.5">
              <label htmlFor="image-steps" className="text-[13px]">Steps</label>
              <Input aria-label="Steps" id="image-steps" type="number" min={1} max={150} placeholder="Backend default" value={session.imageSteps ?? ""}
                onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 1 && n <= 150)) update({ imageSteps: e.target.value ? n : undefined }); }} />
            </div>
            <div className="space-y-1.5">
              <label htmlFor="image-seed" className="text-[13px]">Seed</label>
              <div className="flex gap-1.5">
                <Input aria-label="Seed" id="image-seed" className="font-mono" type="number" min={0} max={4_294_967_295} placeholder="Random" value={session.imageSeed ?? ""}
                  onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 0 && n <= 4_294_967_295)) update({ imageSeed: e.target.value ? n : undefined }); }} />
                <Button variant="outline" className="shrink-0" disabled={session.imageSeed === undefined} onClick={() => update({ imageSeed: undefined })}>Random</Button>
              </div>
            </div>
          </section>
        </SettingsPanel>
      )}
    </div>
  );
}

/** Backend errors arrive as one long line; show a lead and park the rest. */
function RunError({ message }: { message: string }) {
  const long = message.length > ERROR_LEAD_LENGTH;
  return (
    <div role="alert" className="max-w-2xl space-y-1 rounded-lg border border-border bg-secondary/50 px-3 py-2">
      <p className="text-sm">
        {long ? `${message.slice(0, ERROR_LEAD_LENGTH).trimEnd()}…` : message}
      </p>
      {long && (
        <details>
          <summary className="cursor-pointer text-[11px] text-muted-foreground">Show details</summary>
          <pre className="mt-1 whitespace-pre-wrap break-words text-[11px] text-muted-foreground">
            {message}
          </pre>
        </details>
      )}
    </div>
  );
}
