"use client";

import { useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { MoreHorizontal, Search } from "lucide-react";
import { assignUserAction, setUserStatusAction } from "@/app/(dashboard)/users/users-actions";
import { Notice } from "@/components/models/ui";
import { Pill, Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select } from "@/components/ui/select";
import { lastUsed, reachLabel, reachOf, tenantHref, type Reach } from "@/lib/access-model";
import type { AdminUser } from "@/lib/auth/users";
import { cn } from "@/lib/utils";

type Show = "everyone" | "admins" | "portal" | "none";

const ROLE_OPTIONS = [
  { value: "user", label: "User: the portal" },
  { value: "admin", label: "Admin: the dashboard" },
];

const COLS = "grid-cols-[minmax(0,1.5fr)_80px_90px_minmax(0,1fr)_minmax(0,1.2fr)_90px_40px]";

function joined(at: AdminUser["createdAt"]): string {
  if (!at) return "—";
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? "—" : d.toLocaleDateString([], { month: "short", day: "numeric", year: d.getFullYear() === new Date().getFullYear() ? undefined : "numeric" });
}

function signedIn(at: AdminUser["createdAt"]): string {
  if (!at) return "";
  const ms = new Date(at).getTime();
  return Number.isNaN(ms) ? "" : `signed in ${lastUsed(ms)}`;
}

function assign(id: string, role: string, tenantId: string) {
  const fd = new FormData();
  fd.set("id", id);
  fd.set("role", role);
  fd.set("tenantId", tenantId);
  return assignUserAction(fd);
}

/** One waiting account: pick a role and a tenant, then approve. "Not now" leaves it waiting. */
function Approval({ user, tenants, onDone }: { user: AdminUser; tenants: { id: string; name: string }[]; onDone: (text: string | null, error?: boolean) => void }) {
  const [role, setRole] = useState<string>(user.role);
  const [tenantId, setTenantId] = useState(user.tenantId ?? "");
  const [later, setLater] = useState(false);
  const [pending, start] = useTransition();
  if (later) return null;
  const noTenant = role === "user" && !tenantId;
  return (
    <div className="grid items-center gap-3 border-t border-border py-2.5 md:grid-cols-[minmax(0,1fr)_170px_200px_auto_auto]">
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-[13px] font-medium">{user.email}</span>
        <span className="text-[11.5px] text-muted-foreground">{signedIn(user.createdAt)}</span>
      </span>
      <Select aria-label={`Role for ${user.email}`} value={role} onValueChange={setRole} className="h-9 text-[13px]" options={ROLE_OPTIONS} />
      <Select aria-label={`Tenant for ${user.email}`} value={tenantId} onValueChange={setTenantId} searchPlaceholder="Find a tenant" className="h-9 text-[13px]" options={[{ value: "", label: role === "admin" ? "No tenant" : "Pick a tenant" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
      <Button
        type="button"
        size="sm"
        className="h-9"
        disabled={pending}
        title={noTenant ? "Without a tenant they can sign in but reach nothing" : undefined}
        onClick={() => start(async () => {
          const res = await assign(user.id, role, tenantId);
          onDone(res.ok ? `Approved ${user.email}${noTenant ? ". They have no tenant yet, so they can't reach anything." : "."}` : res.error, !res.ok);
        })}
      >
        {pending ? "Approving…" : "Approve"}
      </Button>
      <Button type="button" size="sm" variant="outline" className="h-9" disabled={pending} onClick={() => setLater(true)}>Not now</Button>
    </div>
  );
}

function ChangeDialog({ user, tenants, onClose, onDone }: { user: AdminUser; tenants: { id: string; name: string }[]; onClose: () => void; onDone: (text: string | null, error?: boolean) => void }) {
  const [role, setRole] = useState<string>(user.role);
  const [tenantId, setTenantId] = useState(user.tenantId ?? "");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const reach = reachOf({ role: role as AdminUser["role"], status: "active", tenantId: tenantId || null });
  return (
    <Dialog open onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Change {user.email}</DialogTitle>
          <DialogDescription>A user needs a tenant to use the portal; an admin can use the dashboard with or without one.</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <div className="space-y-1.5">
            <span className="text-[12.5px] font-medium text-secondary-foreground">Role</span>
            <Select aria-label="Role" value={role} onValueChange={setRole} className="h-9 text-[13px]" options={ROLE_OPTIONS} />
          </div>
          <div className="space-y-1.5">
            <span className="text-[12.5px] font-medium text-secondary-foreground">Tenant</span>
            <Select aria-label="Tenant" value={tenantId} onValueChange={setTenantId} searchPlaceholder="Find a tenant" className="h-9 text-[13px]" options={[{ value: "", label: "No tenant" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
          </div>
          <p className="text-[12.5px] text-secondary-foreground">They&apos;ll reach: <b className="font-medium text-foreground">{reachLabel(reach, tenants.find((t) => t.id === tenantId)?.name)}</b></p>
          {error && <p role="alert" className="text-[12.5px] font-medium">{error}</p>}
        </div>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose} disabled={pending}>Cancel</Button>
          <Button
            type="button"
            disabled={pending}
            onClick={() => start(async () => {
              const res = await assign(user.id, role, tenantId);
              if (!res.ok) return setError(res.error);
              onDone(`Saved ${user.email}.`);
              onClose();
            })}
          >
            {pending ? "Saving…" : "Save"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function UsersList({ users, tenants, me }: { users: AdminUser[]; tenants: { id: string; name: string }[]; me: string }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [, start] = useTransition();
  const [query, setQuery] = useState("");
  const [show, setShow] = useState<Show>("everyone");
  const [tenant, setTenant] = useState("");
  const [changing, setChanging] = useState<AdminUser | null>(null);
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const names = useMemo(() => new Map(tenants.map((t) => [t.id, t.name])), [tenants]);

  const waiting = users.filter((u) => u.status !== "active").sort((a, b) => String(b.createdAt ?? "").localeCompare(String(a.createdAt ?? "")));
  const active = users.filter((u) => u.status === "active");
  const admins = active.filter((u) => u.role === "admin");
  const reach = (u: AdminUser): Reach => reachOf(u);
  const stranded = active.filter((u) => reach(u) === "nothing");
  const q = query.trim().toLowerCase();
  const shown = active
    .filter((u) => show === "everyone" || (show === "admins" && u.role === "admin") || (show === "portal" && reach(u) === "portal") || (show === "none" && reach(u) === "nothing"))
    .filter((u) => !tenant || u.tenantId === tenant)
    .filter((u) => !q || `${u.email} ${names.get(u.tenantId ?? "") ?? ""}`.toLowerCase().includes(q))
    .sort((a, b) => Number(b.id === me) - Number(a.id === me) || Number(b.role === "admin") - Number(a.role === "admin") || a.email.localeCompare(b.email));

  const done = (text: string | null, error?: boolean) => {
    if (text) setNotice({ text, strong: error });
    router.refresh();
  };

  function takeAway(u: AdminUser) {
    start(async () => {
      const ok = await confirm({
        title: `Take ${u.email}'s access away?`,
        description: "They go back to waiting for approval: signed in, but able to use nothing. Their keys in the portal stay with the tenant.",
        confirmLabel: "Take access away",
      });
      if (!ok) return;
      const fd = new FormData();
      fd.set("id", u.id);
      fd.set("status", "pending");
      const res = await setUserStatusAction(fd);
      done(res.ok ? `${u.email} is back to waiting for approval.` : res.error, !res.ok);
    });
  }

  const line = [
    `${users.length} ${users.length === 1 ? "person" : "people"}`,
    `${admins.length} admin${admins.length === 1 ? "" : "s"}`,
    waiting.length ? `${waiting.length} waiting for approval` : null,
    stranded.length ? `${stranded.length} can't reach anything yet` : null,
  ].filter(Boolean).join(" · ");

  return (
    <div className="mx-auto flex max-w-[1400px] flex-col gap-[18px]">
      {confirmElement}
      <div className="flex min-w-0 flex-col gap-1.5">
        <h1 className="text-[26px] font-semibold tracking-tight">Users</h1>
        <p className="text-[13px] text-secondary-foreground">{line}</p>
      </div>
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      {waiting.length > 0 && (
        <section aria-label="Waiting for approval" className="rounded-xl border border-muted-foreground/60 bg-card px-[18px] pb-2 pt-3.5">
          <div className="flex flex-wrap items-baseline justify-between gap-2 pb-2">
            <h2 className="text-sm font-semibold">Waiting for approval · {waiting.length}</h2>
            <span className="text-[11.5px] text-muted-foreground">They signed in and can&apos;t do anything until approved</span>
          </div>
          {waiting.map((u) => <Approval key={u.id} user={u} tenants={tenants} onDone={done} />)}
          <p className="border-t border-border py-2.5 text-[11.5px] text-muted-foreground">A user needs a tenant to use the portal; an admin can use the dashboard with or without one.</p>
        </section>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[380px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search people" placeholder="Search email or tenant" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Segmented
          label="Show"
          value={show}
          onChange={setShow}
          options={[
            { value: "everyone", label: "Everyone" },
            { value: "admins", label: "Admins" },
            { value: "portal", label: "Portal users" },
            { value: "none", label: `No access yet${stranded.length ? ` ${stranded.length}` : ""}` },
          ]}
        />
        <Select aria-label="Tenant" value={tenant} onValueChange={setTenant} searchPlaceholder="Find a tenant" className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", tenant && "border-foreground")} options={[{ value: "", label: "Any tenant" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
      </div>

      <section aria-label="People" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[860px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <span>Person</span><span>Role</span><span>Status</span><span>Tenant</span><span>Can reach</span><span>Joined</span><span />
            </div>
            {shown.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{active.length ? "Nobody matches these filters." : "Nobody is approved yet."}</p>}
            {shown.map((u) => {
              const r = reach(u);
              const tname = u.tenantId ? names.get(u.tenantId) : undefined;
              const lastAdmin = u.role === "admin" && admins.length === 1;
              return (
                <div key={u.id} className={cn("grid min-h-[50px] items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px]", COLS)}>
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium">{u.email}</span>
                    {(u.id === me || lastAdmin) && <span className="text-[11.5px] text-muted-foreground">{[u.id === me ? "you" : null, lastAdmin ? "the only admin" : null].filter(Boolean).join(" · ")}</span>}
                  </span>
                  <span><Pill>{u.role === "admin" ? "Admin" : "User"}</Pill></span>
                  <span className="text-secondary-foreground">Active</span>
                  <span className="truncate">
                    {tname ? <Link href={tenantHref({ name: tname })} className="underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{tname}</Link> : <span className="text-muted-foreground">{u.tenantId ? "Deleted tenant" : "No tenant"}</span>}
                  </span>
                  <span className="min-w-0"><Pill inverted={r === "nothing"} className="max-w-full truncate">{reachLabel(r, tname)}</Pill></span>
                  <span className="font-mono text-[12px] text-muted-foreground">{joined(u.createdAt)}</span>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <button type="button" aria-label={`Change ${u.email}`} className="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground"><MoreHorizontal className="h-4 w-4" /></button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onSelect={() => setChanging(u)}>Change role or tenant…</DropdownMenuItem>
                      {u.tenantId && tname && <DropdownMenuItem onSelect={() => router.push(tenantHref({ name: tname }))}>Open {tname}</DropdownMenuItem>}
                      <DropdownMenuSeparator />
                      <DropdownMenuItem disabled={u.id === me || lastAdmin} onSelect={() => takeAway(u)}>
                        {u.id === me ? "Take access away (not your own)" : lastAdmin ? "Take access away (the only admin)" : "Take access away…"}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              );
            })}
          </div>
        </div>
        <p className="border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">The ⋯ menu changes role and tenant, or takes access away (they go back to waiting for approval). You can&apos;t remove the last admin.</p>
      </section>

      {changing && <ChangeDialog user={changing} tenants={tenants} onClose={() => setChanging(null)} onDone={done} />}
    </div>
  );
}
