"use client";

import { useState } from "react";
import { HealthGlyph } from "@/components/overview/ui";
import { STATUS_LABELS, type ModelStatus } from "@/lib/models-model";
import { providerForModel } from "@/lib/model-providers";
import { cn } from "@/lib/utils";
import { StatusDot, StatusText, type StatusGlyph } from "@/components/ui/status";

const MODEL_GLYPH: Record<Exclude<ModelStatus, "down">, StatusGlyph> = { serving: "on", unchecked: "none", maintenance: "off", off: "off" };

/** A model's standing in the shared status marks; failing checks is the DOWN pill. */
export function StatusMark({ status, className, label = true }: { status: ModelStatus; className?: string; label?: boolean }) {
  if (status === "down") return <HealthGlyph state="unhealthy" className={className} />;
  const glyph = MODEL_GLYPH[status];
  return label ? <StatusText glyph={glyph} className={className}>{STATUS_LABELS[status]}</StatusText> : <StatusDot glyph={glyph} label={STATUS_LABELS[status]} className={className} />;
}

/**
 * The model family's mark, grey, so a long list scans by shape. Falls back to
 * the name's first letter when no family is known or the image fails.
 */
export function ProviderMark({ name, upstream, size = 20, className }: { name: string; upstream?: string; size?: number; className?: string }) {
  const provider = providerForModel(name, upstream);
  const [failed, setFailed] = useState(false);
  const letter = name.replace(/[^a-z0-9]/gi, "").slice(0, 1).toUpperCase() || "?";
  return (
    <span
      aria-hidden
      title={provider?.label}
      style={{ width: size, height: size }}
      className={cn("inline-flex shrink-0 items-center justify-center overflow-hidden rounded-md border border-border bg-background", className)}
    >
      {provider && !failed ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={provider.src} alt="" className="h-[70%] w-[70%] object-contain opacity-80 grayscale" onError={() => setFailed(true)} />
      ) : (
        <span className="text-[10px] font-semibold text-muted-foreground">{letter}</span>
      )}
    </span>
  );
}
