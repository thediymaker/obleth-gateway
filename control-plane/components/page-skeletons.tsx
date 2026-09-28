import { Skeleton, SkeletonPanel } from "@/components/ui/skeleton";

/**
 * Route-level loading states. Each mirrors the page it stands in for — the
 * same columns, panel heights and rhythm — so the page arrives without the
 * layout jumping. Everything sweeps with one shared highlight (`.skeleton`).
 */

function Header({ controls = 3, status = true }: { controls?: number; status?: boolean }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-4">
      <div className="space-y-2.5">
        <Skeleton className="h-7 w-56" />
        {status && <Skeleton className="h-4 w-[26rem] max-w-[70vw]" />}
      </div>
      <div className="flex gap-2">
        {Array.from({ length: controls }, (_, i) => <Skeleton key={i} className={i === 0 ? "h-9 w-28 rounded-lg" : "h-9 w-9 rounded-lg"} />)}
      </div>
    </div>
  );
}

function Tiles({ count, className }: { count: number; className: string }) {
  return (
    <div className={className}>
      {Array.from({ length: count }, (_, i) => (
        <SkeletonPanel key={i} className="gap-2.5 px-4 py-3.5">
          <Skeleton className="h-3 w-24" />
          <Skeleton className="h-7 w-20" />
          <Skeleton className="h-3 w-32" />
        </SkeletonPanel>
      ))}
    </div>
  );
}

function Rows({ count, className = "h-4" }: { count: number; className?: string }) {
  return (
    <div className="space-y-3 pt-1">
      {Array.from({ length: count }, (_, i) => <Skeleton key={i} className={className} />)}
    </div>
  );
}

function Loading({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5" aria-busy="true" aria-label={label} role="status">
      <span className="sr-only">{label}</span>
      {children}
    </div>
  );
}

export function OverviewSkeleton() {
  return (
    <Loading label="Loading the overview">
      <Header />
      <Tiles count={5} className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5" />
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_400px]">
        <SkeletonPanel className="h-[380px]"><Skeleton className="h-4 w-32" /><Skeleton className="mt-2 flex-1" /><Skeleton className="h-14" /></SkeletonPanel>
        <SkeletonPanel className="h-[380px]"><Skeleton className="h-4 w-24" /><Skeleton className="h-10 w-40" /><Skeleton className="h-2" /><Rows count={6} className="h-3.5" /></SkeletonPanel>
      </div>
      <SkeletonPanel>
        <Skeleton className="h-4 w-32" />
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 md:grid-cols-4 xl:grid-cols-6 2xl:grid-cols-8">
          {Array.from({ length: 16 }, (_, i) => <Skeleton key={i} className="h-[62px] rounded-[9px]" />)}
        </div>
      </SkeletonPanel>
      <div className="grid gap-4 lg:grid-cols-2">
        <SkeletonPanel><Skeleton className="h-4 w-28" /><Rows count={6} /></SkeletonPanel>
        <SkeletonPanel><Skeleton className="h-4 w-28" /><Rows count={6} /></SkeletonPanel>
      </div>
    </Loading>
  );
}

export function FairshareSkeleton() {
  return (
    <Loading label="Loading fairshare">
      <Header controls={4} />
      <Tiles count={4} className="grid grid-cols-2 gap-3 xl:grid-cols-4" />
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_400px]">
        <SkeletonPanel><Skeleton className="h-4 w-28" /><Skeleton className="h-3 w-72 max-w-full" /><Rows count={8} /></SkeletonPanel>
        <SkeletonPanel><Skeleton className="h-4 w-20" /><Rows count={3} className="h-8" /></SkeletonPanel>
      </div>
      <SkeletonPanel><Skeleton className="h-4 w-36" /><Rows count={6} /></SkeletonPanel>
      <SkeletonPanel className="h-[320px]"><Skeleton className="h-4 w-44" /><Skeleton className="flex-1" /><Skeleton className="h-12" /></SkeletonPanel>
    </Loading>
  );
}

export function GroupsSkeleton() {
  return (
    <Loading label="Loading groups and weights">
      <div className="space-y-2.5"><Skeleton className="h-3 w-28" /><Skeleton className="h-7 w-56" /><Skeleton className="h-4 w-[34rem] max-w-full" /></div>
      <SkeletonPanel><Rows count={5} className="h-10" /></SkeletonPanel>
    </Loading>
  );
}

/** The Models list: header, four tiles, the filter row, and the table. */
export function ModelsSkeleton() {
  return (
    <Loading label="Loading models">
      <Header controls={2} />
      <Tiles count={4} className="grid grid-cols-2 gap-3 xl:grid-cols-4" />
      <div className="flex flex-wrap gap-2">
        <Skeleton className="h-9 w-[26rem] max-w-full rounded-lg" />
        <Skeleton className="h-9 w-80 rounded-lg" />
        <Skeleton className="h-9 w-28 rounded-lg" />
        <Skeleton className="h-9 w-28 rounded-lg" />
      </div>
      <SkeletonPanel className="gap-0 p-0">
        <Skeleton className="m-4 h-3 w-2/3" />
        {Array.from({ length: 10 }, (_, i) => (
          <div key={i} className="flex items-center gap-4 border-t border-border px-4 py-3">
            <Skeleton className="h-5 w-5 rounded-md" />
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-3.5 w-16" />
            <Skeleton className="h-2 w-24" />
            <Skeleton className="ml-auto h-3.5 w-32" />
          </div>
        ))}
      </SkeletonPanel>
    </Loading>
  );
}

/** A model's own page: header, the section list, then the Overview's tiles and chart. */
export function ModelSkeleton() {
  return (
    <Loading label="Loading the model">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="space-y-2.5"><Skeleton className="h-3 w-24" /><Skeleton className="h-8 w-72" /><Skeleton className="h-5 w-[28rem] max-w-[70vw]" /></div>
        <div className="flex gap-2"><Skeleton className="h-9 w-28 rounded-lg" /><Skeleton className="h-9 w-36 rounded-lg" /><Skeleton className="h-9 w-9 rounded-lg" /></div>
      </div>
      <div className="grid gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <div className="hidden space-y-2 lg:block"><Skeleton className="h-9 rounded-lg" /><Rows count={10} className="h-7 rounded-lg" /></div>
        <div className="flex flex-col gap-4">
          <Tiles count={5} className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5" />
          <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
            <SkeletonPanel className="h-[300px]"><Skeleton className="h-4 w-28" /><Skeleton className="mt-2 flex-1" /></SkeletonPanel>
            <div className="flex flex-col gap-4"><SkeletonPanel className="h-[92px]" /><SkeletonPanel className="h-[92px]" /><SkeletonPanel className="h-[92px]" /></div>
          </div>
        </div>
      </div>
    </Loading>
  );
}

/** Request logs: header, search and filters, the per-minute strip, then rows. */
export function LogsSkeleton() {
  return (
    <Loading label="Loading request logs">
      <Header controls={2} />
      <div className="flex flex-wrap gap-2"><Skeleton className="h-9 flex-1 rounded-lg" /><Skeleton className="h-9 w-72 rounded-lg" /></div>
      <div className="flex flex-wrap gap-2">{Array.from({ length: 7 }, (_, i) => <Skeleton key={i} className="h-8 w-28 rounded-lg" />)}</div>
      <SkeletonPanel className="h-[112px]"><Skeleton className="h-3 w-48" /><Skeleton className="flex-1" /></SkeletonPanel>
      <SkeletonPanel className="gap-0 p-0">
        {Array.from({ length: 12 }, (_, i) => (
          <div key={i} className="flex items-center gap-4 border-t border-border px-4 py-3 first:border-t-0">
            <Skeleton className="h-3.5 w-16" /><Skeleton className="h-5 w-10 rounded-full" /><Skeleton className="h-4 w-40" /><Skeleton className="h-4 w-32" /><Skeleton className="ml-auto h-1.5 w-40" />
          </div>
        ))}
      </SkeletonPanel>
    </Loading>
  );
}

/** Reports: header, five tiles, the chart, the two ranked lists, then the table. */
export function ReportsSkeleton() {
  return (
    <Loading label="Loading reports">
      <Header controls={4} />
      <Tiles count={5} className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5" />
      <SkeletonPanel className="h-[340px]"><Skeleton className="h-4 w-32" /><Skeleton className="mt-2 flex-1" /></SkeletonPanel>
      <div className="grid gap-4 lg:grid-cols-2">
        <SkeletonPanel><Skeleton className="h-4 w-28" /><Rows count={6} /></SkeletonPanel>
        <SkeletonPanel><Skeleton className="h-4 w-28" /><Rows count={6} /></SkeletonPanel>
      </div>
      <SkeletonPanel><Skeleton className="h-4 w-24" /><Rows count={8} /></SkeletonPanel>
    </Loading>
  );
}

/** The Playground fills the page edge to edge: sessions, header, surface, composer. */
export function PlaygroundSkeleton() {
  return (
    <div className="flex h-full min-h-[36rem]" aria-busy="true" aria-label="Loading the playground" role="status">
      <span className="sr-only">Loading the playground</span>
      <div className="hidden w-64 shrink-0 flex-col gap-3 border-r border-border bg-card p-3.5 md:flex">
        <Skeleton className="h-9 rounded-lg" />
        <Skeleton className="h-[34px] rounded-lg" />
        <Rows count={7} className="h-10 rounded-lg" />
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex h-14 items-center justify-between border-b border-border px-3">
          <Skeleton className="h-5 w-48" />
          <Skeleton className="h-6 w-72 max-w-[40vw]" />
          <Skeleton className="h-8 w-24 rounded-lg" />
        </div>
        <div className="flex min-h-0 flex-1">
          <div className="flex flex-1 flex-col justify-end gap-4 px-8 pb-4">
            <Skeleton className="ml-auto h-12 w-80 max-w-full rounded-2xl" />
            <Skeleton className="h-24 w-full max-w-3xl" />
            <Skeleton className="mx-auto h-28 w-full max-w-3xl rounded-2xl" />
          </div>
          <div className="hidden w-80 shrink-0 flex-col gap-4 border-l border-border bg-card p-4 lg:flex">
            <Skeleton className="h-4 w-24" />
            <Skeleton className="h-32 rounded-xl" />
            <Skeleton className="h-24" />
            <Rows count={3} className="h-8" />
          </div>
        </div>
      </div>
    </div>
  );
}

/** Every other dashboard page: a header, a row of tiles, and the page's main panels. */
export function DashboardSkeleton() {
  return (
    <Loading label="Loading page">
      <Header controls={2} />
      <Tiles count={4} className="grid grid-cols-2 gap-3 xl:grid-cols-4" />
      <SkeletonPanel><Skeleton className="h-4 w-36" /><Rows count={8} /></SkeletonPanel>
      <SkeletonPanel className="h-56"><Skeleton className="h-4 w-28" /><Skeleton className="flex-1" /></SkeletonPanel>
    </Loading>
  );
}
