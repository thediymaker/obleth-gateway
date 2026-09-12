"use client";

import { useEffect, useState } from "react";
import { Download, Loader2, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { ModelRoute } from "@/lib/obleth";
import type { PlaygroundSession } from "./playground";

// Sizes offered in the picker. Deliberately a superset of the boon's default
// allowed list: the playground drives the image model directly, so it is not
// bound by the boon's schema, and testing a size the boon does not offer is a
// legitimate thing to want to do.
const SIZES = ["256x256", "512x512", "768x768", "1024x1024"] as const;

interface Result {
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

/**
 * Parameterised test bench for `image`-type models. Drives
 * /v1/images/generations through the gateway rather than the chat relay, so
 * size, count, steps, and seed are actually settable.
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
  const [results, setResults] = useState<Result[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const imageModels = models.filter((m) => m.model_type === "image");
  const target = session.imageModel ?? imageModels[0]?.model_name ?? "";
  const prompt = session.imagePrompt ?? "";
  const size = session.imageSize ?? "512x512";
  const count = session.imageCount ?? 1;

  // Results are memory-only, so the session export is the only way to keep
  // them; hook into the same event the chat workspaces use.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<Record<string, unknown>>).detail;
      detail[storageKey] = results;
    };
    window.addEventListener("playground-export", handler);
    return () => window.removeEventListener("playground-export", handler);
  }, [results, storageKey]);

  async function generate() {
    setBusy(true);
    setError(null);
    try {
      const res = await fetch("/api/live/playground/images", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: target,
          prompt: prompt.trim(),
          size,
          n: count,
          ...(session.imageNegativePrompt?.trim()
            ? { negative_prompt: session.imageNegativePrompt.trim() }
            : {}),
          ...(session.imageSteps !== undefined ? { steps: session.imageSteps } : {}),
          ...(session.imageSeed !== undefined ? { seed: session.imageSeed } : {}),
        }),
      });
      const json = (await res.json()) as
        | { images: string[]; latencyMs: number; requestId: string | null }
        | { error: string };
      if (!res.ok || "error" in json) {
        setError("error" in json ? json.error : `Generation failed (${res.status}).`);
        return;
      }
      const perImage = imageModels.find((m) => m.model_name === target)?.cost_per_image ?? 0;
      const at = Date.now();
      setResults((all) => [
        ...json.images.map((url, i) => ({
          id: `${at}-${i}`,
          url,
          model: target,
          prompt: prompt.trim(),
          size,
          seed: session.imageSeed,
          steps: session.imageSteps,
          latencyMs: json.latencyMs,
          costUsd: perImage,
          at,
        })),
        ...all,
      ]);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const optionalNumber = (value: string): number | undefined =>
    value.trim() === "" ? undefined : Number(value);

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 overflow-y-auto p-4 lg:flex-row">
      <div className="w-full shrink-0 space-y-3 lg:w-80">
        <label className="block text-xs font-medium">
          Image model
          <Select
            id="image-target"
            aria-label="Image model"
            className="mt-1"
            value={target}
            onValueChange={(v) => update({ imageModel: v })}
            disabled={loading || imageModels.length === 0}
            placeholder={imageModels.length === 0 ? "No image models registered" : "Select a model"}
            searchPlaceholder="Filter models"
            options={imageModels.map((m) => ({ value: m.model_name, label: m.model_name }))}
          />
        </label>
        <label className="block text-xs font-medium">
          Prompt
          <textarea
            id="image-prompt"
            value={prompt}
            maxLength={4000}
            rows={4}
            onChange={(e) => update({ imagePrompt: e.target.value })}
            placeholder="A watercolour of a lighthouse at dusk"
            className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 text-sm"
          />
        </label>
        <label className="block text-xs font-medium">
          Negative prompt
          <textarea
            id="image-negative-prompt"
            value={session.imageNegativePrompt ?? ""}
            maxLength={4000}
            rows={2}
            onChange={(e) => update({ imageNegativePrompt: e.target.value })}
            placeholder="blurry, watermark"
            className="mt-1 w-full resize-y rounded-md border border-border bg-background p-2 text-sm"
          />
        </label>
        <div className="grid grid-cols-2 gap-3">
          <label className="text-xs font-medium">
            Size
            <Select
              id="image-size"
              aria-label="Size"
              className="mt-1"
              value={size}
              onValueChange={(v) => update({ imageSize: v })}
              options={SIZES.map((s) => ({ value: s, label: s }))}
            />
          </label>
          <label className="text-xs font-medium">
            Count
            <Input
              id="image-count"
              className="mt-1"
              type="number"
              min={1}
              max={4}
              value={count}
              onChange={(e) => update({ imageCount: Math.min(Math.max(Number(e.target.value) || 1, 1), 4) })}
            />
          </label>
          <label className="text-xs font-medium">
            Steps
            <Input
              id="image-steps"
              className="mt-1"
              type="number"
              min={1}
              max={150}
              placeholder="Backend default"
              value={session.imageSteps ?? ""}
              onChange={(e) => update({ imageSteps: optionalNumber(e.target.value) })}
            />
          </label>
          <label className="text-xs font-medium">
            Seed
            <Input
              id="image-seed"
              className="mt-1"
              type="number"
              min={0}
              placeholder="Random"
              value={session.imageSeed ?? ""}
              onChange={(e) => update({ imageSeed: optionalNumber(e.target.value) })}
            />
          </label>
        </div>
        <p className="text-[11px] text-muted-foreground">
          Negative prompt, steps, and seed are not part of the OpenAI images API. They are passed
          through to the backend, which may ignore them.
        </p>
        <Button
          id="image-generate"
          className="w-full"
          onClick={generate}
          disabled={busy || !target || !prompt.trim()}
        >
          {busy ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Sparkles className="mr-2 h-4 w-4" />
          )}
          Generate
        </Button>
        {error && (
          <p role="alert" className="text-xs text-destructive">
            {error}
          </p>
        )}
        <p className="text-[11px] text-muted-foreground">
          Results are held for this tab only. Download anything you want to keep, or use Export
          saved session.
        </p>
      </div>

      <div className="min-w-0 flex-1">
        {results.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No results yet. Set a prompt and generate.
          </p>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {results.map((r) => (
              <figure key={r.id} className="space-y-2 rounded-lg border border-border p-2">
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={r.url}
                  alt={r.prompt}
                  className="w-full rounded-md border border-border bg-secondary/20"
                />
                <figcaption className="space-y-1 text-[11px] text-muted-foreground">
                  <p className="truncate font-medium text-foreground">{r.prompt}</p>
                  <p>
                    {r.model} · {r.size} · {r.latencyMs} ms · ${r.costUsd.toFixed(4)}
                    {r.seed !== undefined && ` · seed ${r.seed}`}
                    {r.steps !== undefined && ` · ${r.steps} steps`}
                  </p>
                  <a
                    href={r.url}
                    download={`playground-${r.id}.png`}
                    className="inline-flex items-center gap-1 underline"
                  >
                    <Download className="h-3 w-3" />
                    Download
                  </a>
                </figcaption>
              </figure>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
