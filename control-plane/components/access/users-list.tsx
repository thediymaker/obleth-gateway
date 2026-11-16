"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { MoreHorizontal, Search } from "lucide-react";
import { assignUserAction, setUserStatusAction } from "@/app/(dashboard)/users/users-actions";
import { Notice } from "@/components/models/ui";
import { Pill, Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select } from "@/components/ui/select";
import { lastUsed, reachLabel, reachOf, tenantHref, type Reach } from "@/lib/access-model";
import type { AdminUser } from "@/lib/auth/users";
import type { MemberRole } from "@/lib/obleth-accounts";
import { cn } from "@/lib/utils";

type Show = "everyone" | "admins" | "portal" | "none";
type TenantRef = { id: string; name: string };
/**
 * Shared tenants a user is a member of; member is false for password accounts,
 * which can't be; tenants is null when they couldn't be loaded.
 */
export type UserTenants = { member: boolean; tenants: (TenantRef & { role: MemberRole })[] | null };

/** Whether the tenant picker may change memberships for this user. */
const editable = (a: UserTenants) => a.member && a.tenants !== null;
const NO_TENANTS: UserTenants = { member: false, tenants: [] };

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

/** Memberships sync only when the selection changed; known is what the admin saw, so only those can be removed. */
function assign(id: string, role: string, access: UserTenants, tenantIds: string[]) {
  const fd = new FormData();
  fd.set("id", id);
  fd.set("role", role);
  const known = access.tenants?.map((t) => t.id) ?? [];
  const changed = known.length !== tenantIds.length || tenantIds.some((t) => !known.includes(t));
  if (editable(access) && changed) {
    fd.set("syncTenants", "1");
    for (const t of tenantIds) fd.append("tenantIds", t);
    for (const t of known) fd.append("knownTenantIds", t);
  }
  return assignUserAction(fd);
}

function TenantPicker({ tenants, value, onChange, disabled, label }: { tenants: TenantRef[]; value: string[]; onChange: (v: string[]) => void; disabled?: boolean; label: string }) {
  const [q, setQ] = useState("");
  const shown = tenants.filter((t) => !q || t.name.toLowerCase().includes(q.trim().toLowerCase()));
  return (
    <div role="group" aria-label={label} className={cn("flex flex-col rounded-lg border border-border", disabled && "opacity-60")}>
      {tenants.length > 6 && (
        <input value={q} onChange={(e) => setQ(e.target.value)} disabled={disabled} aria-label="Find a tenant" placeholder="Find a tenant" className="h-8 border-b border-border bg-transparent px-2.5 text-[12.5px] outline-none placeholder:text-muted-foreground" />
      )}
      <div className="flex max-h-44 flex-col overflow-y-auto py-1">
        {tenants.length === 0 && <span className="px-2.5 py-1.5 text-[12.5px] text-muted-foreground">No shared tenants yet</span>}
        {shown.map((t) => (
          <label key={t.id} className="flex cursor-pointer items-center gap-2 px-2.5 py-1 text-[13px] hover:bg-muted/40">
            <Checkbox checked={value.includes(t.id)} disabled={disabled} onChange={(on) => onChange(on ? [...value, t.id] : value.filter((x) => x !== t.id))} aria-label={t.name} />
            <span className="truncate">{t.name}</span>
          </label>
        ))}
      </div>
    </div>
  );
}

function tenantSummary(ids: string[], tenants: TenantRef[]): string {
  if (ids.length === 0) return "No tenants";
  const names = ids.map((id) => tenants.find((t) => t.id === id)?.name ?? "?");
  return names.length <= 2 ? names.join(", ") : `${names.slice(0, 2).join(", ")} +${names.length - 2}`;
}

/** One waiting account: pick a role and a tenant, then approve. "Not now" leaves it waiting. */
function Approval({ user, tenants, access, onDone }: { user: AdminUser; tenants: TenantRef[]; access: UserTenants; onDone: (text: string | null, error?: boolean) => void }) {
  const [role, setRole] = useState<string>(user.role);
  const [tenantIds, setTenantIds] = useState((access.tenants ?? []).map((t) => t.id));
  const [later, setLater] = useState(false);
  const [pending, start] = useTransition();
  if (later) return null;
  const noTenant = role === "user" && !user.tenantId && tenantIds.length === 0;
  return (
    <div className="grid items-center gap-3 border-t border-border py-2.5 md:grid-cols-[minmax(0,1fr)_170px_200px_auto_auto]">
      <span className="flex min-w-0 flex-col">
        <span className="truncate text-[13px] font-medium">{user.email}</span>
        <span className="text-[11.5px] text-muted-foreground">{signedIn(user.createdAt)}</span>
      </span>
      <Select aria-label={`Role for ${user.email}`} value={role} onValueChange={setRole} className="h-9 text-[13px]" options={ROLE_OPTIONS} />
      <details className="relative">
        <summary aria-label={`Tenants for ${user.email}`} className={cn("flex h-9 cursor-pointer list-none items-center truncate rounded-lg border border-input bg-background px-3 text-[13px]", !editable(access) && "pointer-events-none opacity-60")} title={access.member ? (access.tenants ? undefined : "Tenants couldn't be loaded") : "A password account can't be a tenant member"}>
          {tenantSummary(tenantIds, tenants)}
        </summary>
        <div className="absolute z-20 mt-1 w-64 rounded-lg bg-popover shadow-md">
          <TenantPicker label={`Tenants for ${user.email}`} tenants={tenants} value={tenantIds} onChange={setTenantIds} disabled={!editable(access) || pending} />
        </div>
      </details>
      <Button
        type="button"
        size="sm"
        className="h-9"
        disabled={pending}
        title={noTenant ? "Without a tenant they can sign in but reach nothing" : undefined}
        onClick={() => start(async () => {
          const res = await assign(user.id, role, access, tenantIds);
          onDone(res.ok ? `Approved ${user.email}${noTenant ? ". They have no tenant yet, so they can't reach anything." : "."}` : res.error, !res.ok);
        })}
      >
        {pending ? "Approving…" : "Approve"}
      </Button>
      <Button type="button" size="sm" variant="outline" className="h-9" disabled={pending} onClick={() => setLater(true)}>Not now</Button>
    </div>
  );
}

function ChangeDialog({ user, tenants, access, onClose, onDone }: { user: AdminUser; tenants: TenantRef[]; access: UserTenants; onClose: () => void; onDone: (text: string | null, error?: boolean) => void }) {
  const [role, setRole] = useState<string>(user.role);
  const [tenantIds, setTenantIds] = useState((access.tenants ?? []).map((t) => t.id));
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const reach = reachOf({ role: role as AdminUser["role"], status: "active", tenantId: user.tenantId ?? (tenantIds[0] || null) });
  const leaving = (access.tenants ?? []).filter((t) => !tenantIds.includes(t.id));
  return (
    <Dialog open onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Change {user.email}</DialogTitle>
          <DialogDescription>Admins use the dashboard. Everyone signed in through SSO has the portal with a personal tenant; shared tenants add more.</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3">
          <div className="space-y-1.5">
            <span className="text-[12.5px] font-medium text-secondary-foreground">Role</span>
            <Select aria-label="Role" value={role} onValueChange={setRole} className="h-9 text-[13px]" options={ROLE_OPTIONS} />
          </div>
          <div className="space-y-1.5">
            <span className="text-[12.5px] font-medium text-secondary-foreground">Tenants</span>
            <TenantPicker label="Tenants" tenants={tenants} value={tenantIds} onChange={setTenantIds} disabled={!editable(access) || pending} />
            {!access.member && <p className="text-[12px] text-muted-foreground">A password account can&apos;t be a tenant member.</p>}
            {access.member && !access.tenants && <p className="text-[12px] text-muted-foreground">Tenants couldn&apos;t be loaded, so they won&apos;t change.</p>}
          </div>
          <p className="text-[12.5px] text-secondary-foreground">They&apos;ll reach: <b className="font-medium text-foreground">{reachLabel(reach)}</b></p>
          {leaving.length > 0 && <p className="text-[12.5px] font-medium">Removing them from {leaving.map((t) => t.name).join(", ")} deletes their keys there.</p>}
          {error && <p role="alert" className="text-[12.5px] font-medium">{error}</p>}
        </div>
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose} disabled={pending}>Cancel</Button>
          <Button
            type="button"
            disabled={pending}
            onClick={() => start(async () => {
              const res = await assign(user.id, role, access, tenantIds);
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

export function UsersList({ users, tenants, access, me }: { users: AdminUser[]; tenants: TenantRef[]; access: Record<string, UserTenants>; me: string }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [, start] = useTransition();
  const [query, setQuery] = useState("");
  const [show, setShow] = useState<Show>("everyone");
  const [tenant, setTenant] = useState("");
  const [changing, setChanging] = useState<AdminUser | null>(null);
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const of = (u: AdminUser) => access[u.id] ?? NO_TENANTS;

  const waiting = users.filter((u) => u.status !== "active").sort((a, b) => String(b.createdAt ?? "").localeCompare(String(a.createdAt ?? "")));
  const active = users.filter((u) => u.status === "active");
  const admins = active.filter((u) => u.role === "admin");
  const reach = (u: AdminUser): Reach => reachOf(u);
  const stranded = active.filter((u) => reach(u) === "nothing");
  const q = query.trim().toLowerCase();
  const shown = active
    .filter((u) => show === "everyone" || (show === "admins" && u.role === "admin") || (show === "portal" && reach(u) === "portal") || (show === "none" && reach(u) === "nothing"))
    .filter((u) => !tenant || (of(u).tenants ?? []).some((t) => t.id === tenant) || (!of(u).member && u.tenantId === tenant))
    .filter((u) => !q || `${u.email} ${(of(u).tenants ?? []).map((t) => t.name).join(" ")}`.toLowerCase().includes(q))
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
          {waiting.map((u) => <Approval key={u.id} user={u} tenants={tenants} access={of(u)} onDone={done} />)}
          <p className="border-t border-border py-2.5 text-[11.5px] text-muted-foreground">Admins use the dashboard. SSO users get the portal with a personal tenant; shared tenants are optional.</p>
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
              <span>Person</span><span>Role</span><span>Status</span><span>Tenants</span><span>Can reach</span><span>Joined</span><span />
            </div>
            {shown.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{active.length ? "Nobody matches these filters." : "Nobody is approved yet."}</p>}
            {shown.map((u) => {
              const r = reach(u);
              const ts = of(u).tenants ?? [];
              // A password account can't be a member; it may still carry a legacy single link.
              const legacy = !of(u).member && u.tenantId ? tenants.find((t) => t.id === u.tenantId) : undefined;
              const lastAdmin = u.role === "admin" && admins.length === 1;
              return (
                <div key={u.id} className={cn("grid min-h-[50px] items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px]", COLS)}>
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium">{u.email}</span>
                    {(u.id === me || lastAdmin) && <span className="text-[11.5px] text-muted-foreground">{[u.id === me ? "you" : null, lastAdmin ? "the only admin" : null].filter(Boolean).join(" · ")}</span>}
                  </span>
                  <span><Pill>{u.role === "admin" ? "Admin" : "User"}</Pill></span>
                  <span className="text-secondary-foreground">Active</span>
                  <span className="truncate" title={ts.map((t) => `${t.name} (${t.role})`).join(", ")}>
                    {legacy && <><Link href={tenantHref(legacy)} className="underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{legacy.name}</Link><span className="text-muted-foreground"> · legacy link</span></>}
                    {ts.length === 0 && !legacy && <span className="text-muted-foreground">{of(u).tenants === null ? "Couldn't load" : of(u).member ? "Personal only" : "No tenants"}</span>}
                    {ts.map((t, i) => (
                      <span key={t.id}>
                        {i > 0 && ", "}
                        <Link href={tenantHref(t)} className="underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{t.name}</Link>
                        {t.role !== "member" && <span className="text-muted-foreground"> · {t.role}</span>}
                      </span>
                    ))}
                  </span>
                  <span className="min-w-0"><Pill inverted={r === "nothing"} className="max-w-full truncate">{reachLabel(r)}</Pill></span>
                  <span className="font-mono text-[12px] text-muted-foreground">{joined(u.createdAt)}</span>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <button type="button" aria-label={`Change ${u.email}`} className="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground"><MoreHorizontal className="h-4 w-4" /></button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onSelect={() => setChanging(u)}>Change role or tenants…</DropdownMenuItem>
                      {ts.map((t) => <DropdownMenuItem key={t.id} onSelect={() => router.push(tenantHref(t))}>Open {t.name}</DropdownMenuItem>)}
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
        <p className="border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">The ⋯ menu changes role and tenants, or takes access away (they go back to waiting for approval). You can&apos;t remove the last admin.</p>
      </section>

      {changing && <ChangeDialog user={changing} tenants={tenants} access={of(changing)} onClose={() => setChanging(null)} onDone={done} />}
    </div>
  );
}
