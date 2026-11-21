"use client";

import { useEffect, useRef, useState } from "react";
import { ChevronDown, Download, ImagePlus, Loader2, Pause, Play, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";
import { MeshViewer } from "./mesh-viewer";
import { ModelAvatar, RunError, SectionLabel, Segmented, SettingsPanel } from "./ui";
import { money } from "@/lib/format";

export const MESH_QUALITIES = [
  { value: "512", label: "Fast", hint: "512 voxels: about a third of the time, softer detail" },
  { value: "1024", label: "Standard", hint: "1024 voxels: the usual choice" },
  { value: "1536", label: "High", hint: "1536 voxels: finest detail, slowest" },
] as const;
const FACES = [100_000, 250_000, 500_000, 1_000_000] as const;
const TEXTURES = [1024, 2048, 4096] as const;
const POLL_MS = 3000;
const ACCEPT = "image/png,image/jpeg,image/webp";

type Quality = (typeof MESH_QUALITIES)[number]["value"];
type JobMeta = NonNullable<PlaygroundSession["meshJobs"]>[number];

/** What the page knows about a job right now; memory-only, rebuilt by polling. */
interface Live {
  status: "queued" | "in_progress" | "completed" | "failed" | "gone";
  stage?: string;
  progress?: number;
  queuePosition?: number;
  faces?: number;
  completedAt?: number;
  error?: string;
  glbUrl?: string;
}

// An image handed over from another mode (Image's "Make 3D"), read once when
// the workspace mounts. Module state rather than session state: a generated
// image is megabytes of base64, too big for the localStorage the session
// list lives in.
let handoff: { url: string; name: string } | null = null;

export function handOffToMesh(url: string, name = "image.png") {
  handoff = { url, name };
}

const fmtFaces = (n: number) => (n >= 1_000_000 ? `${n / 1_000_000}M` : `${Math.round(n / 1000)}k`);
const fmtElapsed = (ms: number) => {
  const s = Math.max(0, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
};
const stageLabel = (live?: Live) => {
  if (!live || live.status === "queued") return live?.queuePosition && live.queuePosition > 1 ? `Queued, ${live.queuePosition - 1} ahead` : "Queued";
  const stage = live.stage && live.stage !== "in_progress" ? live.stage : "working";
  return stage.charAt(0).toUpperCase() + stage.slice(1).replace(/_/g, " ");
};

/**
 * Test bench for `mesh`-type models: an image in, a textured 3D model out,
 * through the gateway's /v1/meshes job API.
 *
 * A job takes minutes, so the session keeps the job ids (small) and the page
 * resumes polling them after a mode switch or a reload. The source images and
 * the downloaded meshes stay in memory only, for the same quota reason as the
 * image gallery; a finished mesh is fetched again from the gateway while the
 * backend still holds it.
 */
export function MeshWorkspace({
  session,
  update,
  models,
  loading,
  settingsOpen = true,
}: {
  session: PlaygroundSession;
  update: (patch: Partial<PlaygroundSession>) => void;
  models: ModelRoute[];
  loading: boolean;
  settingsOpen?: boolean;
}) {
  const meshModels = models.filter((m) => m.model_type === "mesh");
  const target = session.meshModel ?? meshModels[0]?.model_name ?? "";
  const quality: Quality = session.meshResolution ?? "1024";
  const faces = session.meshFaces ?? 500_000;
  const texture = session.meshTexture ?? 2048;
  const jobs = session.meshJobs ?? [];
  const perMesh = meshModels.find((m) => m.model_name === target)?.cost_per_video ?? 0;

  const [source, setSource] = useState<{ file: File; url: string } | null>(null);
  const [live, setLive] = useState<Record<string, Live>>({});
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [spin, setSpin] = useState(true);
  const [now, setNow] = useState(() => Date.now());
  const picker = useRef<HTMLInputElement>(null);
  const urls = useRef<string[]>([]);
  const fetching = useRef(new Set<string>());
  const jobsRef = useRef(jobs);
  jobsRef.current = jobs;
  const liveRef = useRef(live);
  liveRef.current = live;

  const keepUrl = (u: string) => { urls.current.push(u); return u; };
  useEffect(() => () => urls.current.forEach((u) => URL.revokeObjectURL(u)), []);

  const choose = (file: File | undefined | null) => {
    if (!file) return;
    if (!ACCEPT.split(",").includes(file.type)) { setSubmitError("Use a PNG, JPEG or WebP image."); return; }
    setSubmitError(null);
    setSource({ file, url: keepUrl(URL.createObjectURL(file)) });
  };

  // Take an image another mode handed over.
  useEffect(() => {
    const h = handoff;
    handoff = null;
    if (!h) return;
    void fetch(h.url).then((r) => r.blob()).then((b) => choose(new File([b], h.name, { type: b.type || "image/png" }))).catch(() => setSubmitError("Could not load the handed-over image."));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Poll every unfinished job; download each finished one once.
  useEffect(() => {
    let stopped = false;
    const poll = async () => {
      for (const job of jobsRef.current) {
        const known = liveRef.current[job.id];
        if (known && (known.status === "failed" || known.status === "gone" || (known.status === "completed" && known.glbUrl))) continue;
        if (known?.status !== "completed") {
          const res = await fetch(`/api/live/playground/meshes/${encodeURIComponent(job.id)}`).catch(() => null);
          if (stopped) return;
          if (!res) continue;
          if (res.status === 404) { setLive((l) => ({ ...l, [job.id]: { status: "gone" } })); continue; }
          const json = (await res.json().catch(() => null)) as { job?: Record<string, unknown>; error?: string } | null;
          const j = json?.job;
          if (!res.ok || !j) continue;
          const next: Live = {
            status: (["queued", "in_progress", "completed", "failed"].includes(String(j.status)) ? j.status : "in_progress") as Live["status"],
            stage: typeof j.stage === "string" ? j.stage : undefined,
            progress: typeof j.progress === "number" ? j.progress : undefined,
            queuePosition: typeof j.queue_position === "number" ? j.queue_position : undefined,
            faces: typeof j.faces === "number" ? j.faces : undefined,
            completedAt: typeof j.completed_at === "number" ? j.completed_at * 1000 : undefined,
            error: typeof (j.error as { message?: unknown } | null)?.message === "string" ? (j.error as { message: string }).message : typeof j.error === "string" ? j.error : undefined,
          };
          setLive((l) => ({ ...l, [job.id]: { ...l[job.id], ...next } }));
          if (next.status !== "completed") continue;
        }
        if (fetching.current.has(job.id)) continue;
        fetching.current.add(job.id);
        void fetch(`/api/live/playground/meshes/${encodeURIComponent(job.id)}/content`)
          .then(async (r) => {
            if (!r.ok) throw new Error(r.status === 404 ? "gone" : `download failed (${r.status})`);
            const url = keepUrl(URL.createObjectURL(await r.blob()));
            setLive((l) => ({ ...l, [job.id]: { ...l[job.id], status: "completed", glbUrl: url } }));
          })
          .catch((e: Error) => setLive((l) => ({ ...l, [job.id]: e.message === "gone" ? { status: "gone" } : { ...l[job.id], status: "failed", error: e.message } })))
          .finally(() => fetching.current.delete(job.id));
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    const clock = setInterval(() => setNow(Date.now()), 1000);
    return () => { stopped = true; clearInterval(timer); clearInterval(clock); };
  }, []);

  const busy = jobs.some((j) => { const s = live[j.id]?.status; return !s || s === "queued" || s === "in_progress"; });

  async function submit() {
    if (submitting || !target || !source) return;
    setSubmitting(true);
    setSubmitError(null);
    const form = new FormData();
    form.set("model", target);
    form.set("image", source.file, source.file.name || "image.png");
    form.set("resolution", quality);
    form.set("faces", String(faces));
    form.set("texture_size", String(texture));
    if (session.meshSeed !== undefined) form.set("seed", String(session.meshSeed));
    try {
      const res = await fetch("/api/live/playground/meshes", { method: "POST", body: form });
      const json = (await res.json().catch(() => null)) as { job?: { id?: string; seed?: number }; error?: string } | null;
      const id = json?.job?.id;
      if (!res.ok || !id) { setSubmitError(json?.error ?? `The job could not be started (${res.status}).`); return; }
      const meta: JobMeta = {
        id, model: target, resolution: quality, faces, texture,
        seed: typeof json?.job?.seed === "number" ? json.job.seed : session.meshSeed,
        createdAt: Date.now(), sourceName: source.file.name.slice(0, 200),
      };
      setThumbs((t) => ({ ...t, [id]: source.url }));
      setLive((l) => ({ ...l, [id]: { status: "queued" } }));
      update({
        meshJobs: [...jobs, meta].slice(-20),
        ...(session.title === "Untitled session" ? { title: `3D: ${source.file.name.replace(/\.[a-z]+$/i, "").slice(0, 50) || "model"}` } : {}),
      });
    } catch (e) {
      setSubmitError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  const remove = (id: string) => update({ meshJobs: jobs.filter((j) => j.id !== id) });
  const chip = "inline-flex h-[30px] items-center gap-1.5 rounded-lg border border-border px-2.5 text-[12.5px] text-secondary-foreground hover:bg-accent disabled:opacity-60";

  return (
    <div className="relative flex h-full min-h-0">
      <section aria-label="3D models" className="flex min-w-0 flex-1 flex-col">
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-6 md:px-8">
          <div className="mx-auto flex min-h-full max-w-4xl flex-col gap-7">
            {!jobs.length && (
              <div className="m-auto flex max-w-lg flex-col items-center gap-3 py-10 text-center">
                <h2 className="text-lg font-medium">Turn a picture into a 3D model.</h2>
                <p className="text-sm text-muted-foreground">
                  {meshModels.length === 0
                    ? "No 3D models are registered yet. Add one with the 3D mesh type on the Models page to start."
                    : "Drop in a picture of one object. The job runs on the gateway like any client's, takes a few minutes, and ends in a textured model you can turn around here and download as a .glb."}
                </p>
              </div>
            )}
            {[...jobs].reverse().map((job) => {
              const l = live[job.id];
              const done = l?.status === "completed";
              const elapsed = (l?.completedAt ?? now) - job.createdAt;
              return (
                <article key={job.id} aria-label={`${job.model} 3D result`} className="flex flex-col gap-2.5">
                  <div className="flex flex-wrap items-center gap-2">
                    {thumbs[job.id]
                      // eslint-disable-next-line @next/next/no-img-element
                      ? <img src={thumbs[job.id]} alt="" className="h-10 w-10 rounded-lg border border-border object-cover" />
                      : <span className="flex h-10 w-10 items-center justify-center rounded-lg border border-dashed border-border text-muted-foreground"><ImagePlus className="h-4 w-4" aria-hidden="true" /></span>}
                    <div className="min-w-0 flex-1 space-y-0.5">
                      <p className="truncate text-sm">{job.sourceName || "Image"}</p>
                      <div className="flex flex-wrap gap-x-3 font-mono text-[11.5px] text-muted-foreground">
                        <span>{job.model}</span>
                        <span>{MESH_QUALITIES.find((q) => q.value === job.resolution)?.label ?? job.resolution}</span>
                        {job.faces && <span>{fmtFaces(l?.faces ?? job.faces)} faces</span>}
                        {job.texture && <span>{job.texture}px texture</span>}
                        <span>seed {job.seed ?? "random"}</span>
                        {l?.status !== "gone" && <span>{fmtElapsed(elapsed)}</span>}
                        {perMesh > 0 && <span>{money(perMesh, { precise: true })}</span>}
                      </div>
                    </div>
                    {done && l.glbUrl && (
                      <>
                        <Button variant="ghost" size="icon" className="h-8 w-8 text-muted-foreground" title={spin ? "Stop turning" : "Turn"} aria-label={spin ? "Stop turning" : "Turn"} onClick={() => setSpin(!spin)}>{spin ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}</Button>
                        <a href={l.glbUrl} download={`${job.id}.glb`} className="inline-flex h-8 items-center gap-1.5 rounded-md border border-border px-2.5 text-[12.5px] hover:bg-accent"><Download className="h-3.5 w-3.5" />Download .glb</a>
                      </>
                    )}
                    <Button variant="ghost" size="icon" className="h-8 w-8 text-muted-foreground" title="Remove from this session" aria-label="Remove from this session" onClick={() => remove(job.id)}><X className="h-3.5 w-3.5" /></Button>
                  </div>
                  {l?.status === "failed" && <RunError message={l.error ?? "The job failed."} />}
                  {l?.status === "gone" && <p className="text-xs text-muted-foreground">The backend no longer holds this job (finished jobs are kept for a limited time). Run it again to get the model back.</p>}
                  {(!l || l.status === "queued" || l.status === "in_progress" || (done && !l.glbUrl)) && (
                    <div className="flex aspect-[16/9] max-w-3xl flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-border bg-card px-6">
                      <span className="inline-flex items-center gap-2 text-sm"><Loader2 className="h-4 w-4 animate-spin" />{done ? "Downloading" : stageLabel(l)}</span>
                      <div className="h-1.5 w-full max-w-sm overflow-hidden rounded-full bg-secondary" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={l?.progress ?? 0}>
                        <div className="h-full rounded-full bg-foreground transition-[width] duration-700" style={{ width: `${done ? 100 : Math.max(2, l?.progress ?? 0)}%` }} />
                      </div>
                      <span className="font-mono text-[11.5px] text-muted-foreground">{done ? "100%" : `${l?.progress ?? 0}%`} · {fmtElapsed(elapsed)}</span>
                    </div>
                  )}
                  {done && l.glbUrl && <MeshViewer url={l.glbUrl} autoRotate={spin} className="aspect-[16/9] max-w-3xl rounded-xl border border-border bg-gradient-to-b from-card to-secondary/40" />}
                </article>
              );
            })}
          </div>
        </div>

        <div className="px-4 pb-4 md:px-8">
          <div
            className={cn("mx-auto max-w-3xl rounded-2xl border bg-card", dragging ? "border-foreground" : "border-border")}
            onDragOver={(e) => { e.preventDefault(); setDragging(true); }}
            onDragLeave={() => setDragging(false)}
            onDrop={(e) => { e.preventDefault(); setDragging(false); choose(e.dataTransfer.files[0]); }}
            onPaste={(e) => choose(Array.from(e.clipboardData.files)[0])}
          >
            <input ref={picker} type="file" accept={ACCEPT} className="hidden" aria-label="Source image" onChange={(e) => { choose(e.target.files?.[0]); e.target.value = ""; }} />
            <button type="button" onClick={() => picker.current?.click()} className="flex w-full items-center gap-3 px-4 pb-1 pt-3.5 text-left">
              {source
                // eslint-disable-next-line @next/next/no-img-element
                ? <img src={source.url} alt="Source" className="h-14 w-14 rounded-lg border border-border object-cover" />
                : <span className="flex h-14 w-14 items-center justify-center rounded-lg border border-dashed border-border text-muted-foreground"><ImagePlus className="h-5 w-5" aria-hidden="true" /></span>}
              <span className="flex flex-col gap-0.5">
                <span className="text-sm">{source ? source.file.name : "Choose, drop or paste an image"}</span>
                <span className="text-xs text-muted-foreground">One object, ideally on a plain background. PNG, JPEG or WebP.</span>
              </span>
            </button>
            <div className="flex flex-wrap items-center gap-1.5 px-2.5 pb-2.5 pt-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} disabled={loading || meshModels.length === 0} aria-label={`3D model: ${target || "none"}`}>
                    <ModelAvatar name={target || "?"} size="xs" /><span className="max-w-40 truncate">{target || "No model"}</span><ChevronDown className="h-3.5 w-3.5" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {meshModels.map((m) => <DropdownMenuItem key={m.model_name} onSelect={() => update({ meshModel: m.model_name })}>{m.model_name}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button type="button" className={chip} aria-label={`Quality: ${quality}`}>{MESH_QUALITIES.find((q) => q.value === quality)?.label}<ChevronDown className="h-3.5 w-3.5" /></button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {MESH_QUALITIES.map((q) => <DropdownMenuItem key={q.value} onSelect={() => update({ meshResolution: q.value })}>{q.label}</DropdownMenuItem>)}
                </DropdownMenuContent>
              </DropdownMenu>
              <div className="flex-1" />
              {perMesh > 0 && <span className="font-mono text-[11.5px] text-muted-foreground" title="List price per mesh">≈ {money(perMesh, { precise: true })}</span>}
              <Button className="h-9" disabled={submitting || !target || !source} onClick={() => void submit()}>
                {submitting && <Loader2 className="h-4 w-4 animate-spin" />}
                Generate 3D
              </Button>
            </div>
          </div>
          {submitError && <div className="mx-auto mt-2 max-w-3xl"><RunError message={submitError} /></div>}
          <p className="mx-auto mt-2 max-w-3xl text-center text-xs text-muted-foreground">
            {busy ? "You can switch tabs or reload; running jobs pick up where they were." : "Models stay available while the backend keeps them. Download anything you want to keep."}
          </p>
        </div>
      </section>

      {settingsOpen && (
        <SettingsPanel
          title="3D settings"
          label="3D settings"
          action={<Button variant="ghost" size="sm" className="h-7 text-xs text-muted-foreground" onClick={() => update({ meshResolution: undefined, meshFaces: undefined, meshTexture: undefined, meshSeed: undefined })}>Reset to defaults</Button>}
        >
          <section className="space-y-2.5">
            <SectionLabel htmlFor="mesh-target">Model</SectionLabel>
            <Select
              id="mesh-target"
              aria-label="3D model"
              className="w-full"
              value={target}
              onValueChange={(v) => update({ meshModel: v })}
              disabled={loading || meshModels.length === 0}
              placeholder={meshModels.length === 0 ? "No 3D models registered" : "Select a model"}
              searchPlaceholder="Filter models"
              options={meshModels.map((m) => ({ value: m.model_name, label: m.model_name }))}
            />
            {target && <p className="text-xs text-muted-foreground">{perMesh > 0 ? `${money(perMesh, { precise: true })} per mesh` : "No price set"}</p>}
          </section>

          <section className="space-y-2.5">
            <SectionLabel>Quality</SectionLabel>
            <Segmented label="Quality" value={quality} onChange={(v) => update({ meshResolution: v as Quality })} options={MESH_QUALITIES.map((q) => ({ value: q.value, label: q.label }))} />
            <p className="text-[11.5px] leading-snug text-muted-foreground">{MESH_QUALITIES.find((q) => q.value === quality)?.hint}</p>
          </section>

          <section className="space-y-2.5">
            <SectionLabel>Detail</SectionLabel>
            <Segmented label="Faces" value={String(faces)} onChange={(v) => update({ meshFaces: Number(v) })} options={FACES.map((n) => ({ value: String(n), label: fmtFaces(n) }))} />
            <p className="text-[11.5px] leading-snug text-muted-foreground">Triangles in the exported model. Fewer load faster in game engines; more keep fine detail.</p>
          </section>

          <section className="space-y-2.5">
            <SectionLabel>Texture</SectionLabel>
            <Segmented label="Texture size" value={String(texture)} onChange={(v) => update({ meshTexture: Number(v) })} options={TEXTURES.map((n) => ({ value: String(n), label: `${n / 1024}K` }))} />
          </section>

          <section className="space-y-1.5 border-t border-border pt-4">
            <label htmlFor="mesh-seed" className="text-[13px]">Seed</label>
            <div className="flex gap-1.5">
              <Input aria-label="Seed" id="mesh-seed" className="font-mono" type="number" min={0} max={4_294_967_295} placeholder="Random" value={session.meshSeed ?? ""}
                onChange={(e) => { const n = e.target.valueAsNumber; if (!e.target.value || (Number.isInteger(n) && n >= 0 && n <= 4_294_967_295)) update({ meshSeed: e.target.value ? n : undefined }); }} />
              <Button variant="outline" className="shrink-0" disabled={session.meshSeed === undefined} onClick={() => update({ meshSeed: undefined })}>Random</Button>
            </div>
            <p className="text-[11.5px] leading-snug text-muted-foreground">Quality, detail, texture and seed are passed to the backend, which may ignore ones it does not support.</p>
          </section>
        </SettingsPanel>
      )}
    </div>
  );
}
