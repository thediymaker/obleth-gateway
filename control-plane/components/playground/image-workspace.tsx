"use client";

import { useEffect, useRef, useState } from "react";
import { Download, Loader2, RotateCcw, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import type { PlaygroundSession } from "./playground";

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
 * Laid out as toolbar / timeline / composer, the same three bands
 * `workspaces.tsx` uses for chat: the prompt is the composer, the settings you
 * change constantly are the toolbar, and the rest live in the header's
 * Parameters panel (`playground.tsx`). An earlier version put every field in a
 * 320px rail beside the results, which read as a third sidebar on a page that
 * already has two.
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
}: {
  session: PlaygroundSession;
  update: (patch: Partial<PlaygroundSession>) => void;
  models: ModelRoute[];
  loading: boolean;
  storageKey: string;
}) {
  const [runs, setRuns] = useState<Run[]>([]);
  const timeline = useRef<HTMLDivElement>(null);

  const imageModels = models.filter((m) => m.model_type === "image");
  const target = session.imageModel ?? imageModels[0]?.model_name ?? "";
  const prompt = session.imagePrompt ?? "";
  const size = session.imageSize ?? "512x512";
  const count = session.imageCount ?? 1;
  const busy = runs.some((r) => r.status === "busy");

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
      el.scrollTop = el.scrollHeight;
    }
  });

  const patch = (id: string, next: Partial<Run>) =>
    setRuns((all) => all.map((r) => (r.id === id ? { ...r, ...next } : r)));

  async function generate(params: RunParams) {
    const id = `${Date.now()}-${crypto.randomUUID()}`;
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
      const perImage = imageModels.find((m) => m.model_name === params.model)?.cost_per_image ?? 0;
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
          costUsd: perImage,
          at,
        })),
      });
    } catch (e) {
      patch(id, { status: "error", error: String(e) });
    }
  }

  // Snapshot the toolbar and Parameters panel at submit time. A run keeps its
  // own copy so Retry reproduces the request that produced that row, not
  // whatever the controls happen to say later.
  const submit = () => {
    if (busy || !target || !prompt.trim()) return;
    if (session.title === "Untitled session") {
      update({ title: prompt.trim().slice(0, 60) });
    }
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

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-3">
        <Select
          id="image-target"
          aria-label="Image model"
          className="w-52"
          value={target}
          onValueChange={(v) => update({ imageModel: v })}
          disabled={loading || imageModels.length === 0}
          placeholder={imageModels.length === 0 ? "No image models registered" : "Select a model"}
          searchPlaceholder="Filter models"
          options={imageModels.map((m) => ({ value: m.model_name, label: m.model_name }))}
        />
        <Select
          id="image-size"
          aria-label="Size"
          className="w-32"
          value={size}
          onValueChange={(v) => update({ imageSize: v })}
          options={SIZES.map((s) => ({ value: s, label: s }))}
        />
        <Select
          id="image-count"
          aria-label="Images per generation"
          className="w-32"
          value={String(count)}
          onValueChange={(v) => update({ imageCount: Number(v) })}
          options={COUNTS.map((n) => ({ value: String(n), label: n === 1 ? "1 image" : `${n} images` }))}
        />
        <div className="ml-auto">
          <Button variant="ghost" size="sm" disabled={!runs.length} onClick={() => setRuns([])}>
            Clear
          </Button>
        </div>
      </div>

      <div
        ref={timeline}
        className="min-h-0 flex-1 space-y-8 overflow-y-auto p-4 md:p-6"
        aria-label="Generation timeline"
      >
        {!runs.length && (
          <div className="mx-auto flex min-h-full max-w-lg flex-col items-center justify-center gap-3 text-center">
            <h2 className="text-lg font-medium">Describe a picture.</h2>
            <p className="text-sm text-muted-foreground">
              Generations run against the image model you pick above, straight through the gateway,
              so size, count, steps, and seed all apply. Each result shows its latency and cost.
            </p>
          </div>
        )}
        {runs.map((run) => (
          <div key={run.id} className="space-y-3">
            <div className="ml-auto max-w-3xl rounded-xl border border-violet-500/20 bg-violet-500/10 px-4 py-3">
              <p className="mb-1 text-xs font-medium text-muted-foreground">You</p>
              <p className="whitespace-pre-wrap break-words text-sm">{run.prompt}</p>
            </div>
            <section
              aria-label={`${run.model} result`}
              className="min-w-0 overflow-hidden rounded-xl border border-border bg-secondary/10"
            >
              <div className="flex items-center justify-between gap-2 border-b border-border px-3 py-2">
                <span className="truncate text-xs font-semibold">{run.model}</span>
                <Button
                  variant="ghost"
                  size="icon"
                  title="Generate this prompt again"
                  disabled={busy}
                  onClick={() => void generate(run)}
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                </Button>
              </div>
              <div className="p-3">
                {run.status === "busy" && (
                  <p className="flex items-center gap-2 text-sm text-muted-foreground">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    Generating {run.count === 1 ? "an image" : `${run.count} images`} at {run.size}…
                  </p>
                )}
                {run.status === "error" && <RunError message={run.error ?? "Generation failed."} />}
                {run.status === "done" && (
                  <>
                    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                      {run.images.map((image) => (
                        <figure key={image.id} className="space-y-2">
                          {/* eslint-disable-next-line @next/next/no-img-element */}
                          <img
                            src={image.url}
                            alt={image.prompt}
                            className="w-full rounded-md border border-border bg-secondary/20"
                          />
                          <figcaption>
                            <a
                              href={image.url}
                              download={`playground-${image.id}.png`}
                              className="inline-flex items-center gap-1 text-[11px] text-muted-foreground underline"
                            >
                              <Download className="h-3 w-3" />
                              Download
                            </a>
                          </figcaption>
                        </figure>
                      ))}
                    </div>
                    <p className="mt-3 text-[11px] text-muted-foreground">
                      {run.model} · {run.size} · {run.images[0]?.latencyMs} ms · $
                      {run.images.reduce((total, i) => total + i.costUsd, 0).toFixed(4)}
                      {run.seed !== undefined && ` · seed ${run.seed}`}
                      {run.steps !== undefined && ` · ${run.steps} steps`}
                      {run.negativePrompt && ` · negative: ${run.negativePrompt}`}
                    </p>
                  </>
                )}
              </div>
            </section>
          </div>
        ))}
      </div>

      <div className="space-y-2 border-t border-border bg-background px-4 py-3">
        <div className="flex items-end gap-2 rounded-xl border border-border bg-secondary/10 p-2 focus-within:ring-1 focus-within:ring-ring">
          <textarea
            id="image-prompt"
            aria-label="Prompt"
            rows={2}
            maxLength={4000}
            className="min-w-0 flex-1 resize-none bg-transparent p-2 text-sm outline-none"
            value={prompt}
            onChange={(e) => update({ imagePrompt: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                submit();
              }
            }}
            placeholder={
              imageModels.length === 0
                ? "Register an image model to begin…"
                : "A watercolour of a lighthouse at dusk…"
            }
          />
          <Button id="image-generate" disabled={busy || !target || !prompt.trim()} onClick={submit}>
            {busy ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : (
              <Sparkles className="mr-2 h-4 w-4" />
            )}
            Generate
          </Button>
        </div>
        <p className="text-[11px] text-muted-foreground">
          The prompt stays put so you can adjust it and generate again. Results are held for this
          tab only — download anything you want to keep, or use Export saved session.
        </p>
      </div>
    </div>
  );
}

/** Backend errors arrive as one long line; show a lead and park the rest. */
function RunError({ message }: { message: string }) {
  const long = message.length > ERROR_LEAD_LENGTH;
  return (
    <div role="alert" className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2">
      <p className="text-sm text-destructive">
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
