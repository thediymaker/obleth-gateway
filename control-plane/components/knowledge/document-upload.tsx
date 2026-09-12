"use client";

import { useRef, useState } from "react";
import { UploadCloud } from "lucide-react";
import { formatBytes } from "@/lib/format";
import type { KnowledgeDocument } from "@/lib/obleth";
import { cn } from "@/lib/utils";

/** Read a File as base64 via the browser's own data-URL encoder, which
 * handles arbitrary file sizes without the call-stack limits of manually
 * chunking bytes through String.fromCharCode. */
function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("failed to read file"));
    reader.onload = () => {
      const result = reader.result as string; // "data:<mime>;base64,<data>"
      const idx = result.indexOf(",");
      resolve(idx >= 0 ? result.slice(idx + 1) : result);
    };
    reader.readAsDataURL(file);
  });
}

export function DocumentUpload({
  collectionId,
  maxUploadBytes,
  onUploaded,
  onSettled,
}: {
  collectionId: string;
  /** Decoded-byte limit enforced server-side, or null if unknown. */
  maxUploadBytes: number | null;
  /** Called per successfully uploaded file, as it lands. */
  onUploaded: (doc: KnowledgeDocument) => void;
  /** Called once after a batch finishes with at least one upload, so callers
   * refresh server-derived data (chunk counts, collection status) a single
   * time rather than once per file. */
  onSettled?: () => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [dragOver, setDragOver] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [errors, setErrors] = useState<string[]>([]);

  async function uploadOne(file: File) {
    const content_base64 = await readFileAsBase64(file);
    const res = await fetch(`/api/live/knowledge/collections/${collectionId}/documents`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title: file.name, filename: file.name, content_base64 }),
    });
    const body = await res.json().catch(() => null);
    if (!res.ok) {
      throw new Error(
        (body && typeof body.error === "string" && body.error) ||
          `upload failed (HTTP ${res.status})`,
      );
    }
    onUploaded(body as KnowledgeDocument);
  }

  async function handleFiles(list: FileList | null) {
    const files = Array.from(list ?? []);
    if (files.length === 0) return;
    setErrors([]);

    // Files go up one at a time: each is base64-encoded in full before it is
    // sent, so a parallel batch would hold every file in memory at once and
    // fire N multi-megabyte bodies at the gateway simultaneously. One file
    // failing doesn't abort the batch — its reason is collected and the rest
    // still upload.
    const failures: string[] = [];
    for (let i = 0; i < files.length; i++) {
      const file = files[i];
      setProgress({ done: i, total: files.length });

      // Pre-check against the decoded-byte limit before paying for a
      // multi-megabyte base64 encode and request that the server will reject
      // anyway. The server check remains authoritative.
      if (maxUploadBytes != null && file.size > maxUploadBytes) {
        failures.push(
          `"${file.name}" is ${formatBytes(file.size)}, over the ${formatBytes(maxUploadBytes)} limit.`,
        );
        continue;
      }

      try {
        await uploadOne(file);
      } catch (e) {
        failures.push(`"${file.name}": ${e instanceof Error ? e.message : String(e)}`);
      }
    }

    setProgress(null);
    setErrors(failures);
    if (inputRef.current) inputRef.current.value = "";
    if (failures.length < files.length) onSettled?.();
  }

  const pending = progress !== null;

  return (
    <div>
      <div
        role="button"
        tabIndex={0}
        onClick={() => inputRef.current?.click()}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") inputRef.current?.click();
        }}
        onDragOver={(e) => {
          e.preventDefault();
          setDragOver(true);
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragOver(false);
          void handleFiles(e.dataTransfer.files);
        }}
        className={cn(
          "flex cursor-pointer flex-col items-center justify-center gap-1.5 rounded-lg border border-dashed px-6 py-8 text-center transition-colors",
          dragOver ? "border-primary/60 bg-primary/5" : "border-border/70 bg-background/30 hover:border-border",
          pending && "pointer-events-none opacity-60",
        )}
      >
        <UploadCloud className="h-6 w-6 text-muted-foreground/70" />
        <p className="text-sm font-medium">
          {progress
            ? progress.total === 1
              ? "Uploading…"
              : `Uploading ${progress.done + 1} of ${progress.total}…`
            : "Drop files here, or click to browse"}
        </p>
        <p className="text-xs text-muted-foreground">
          {maxUploadBytes != null
            ? `Up to ${formatBytes(maxUploadBytes)} per file`
            : "Any size the gateway accepts"}
        </p>
      </div>
      <input
        ref={inputRef}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => void handleFiles(e.target.files)}
      />
      {errors.length > 0 && (
        <div className="mt-2 space-y-1 rounded-md border border-destructive/35 bg-destructive/10 px-3 py-2 text-xs text-destructive">
          {errors.length > 1 && (
            <p className="font-medium">{errors.length} files were not uploaded</p>
          )}
          {errors.map((err, i) => (
            <p key={i} className="leading-relaxed">
              {err}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
