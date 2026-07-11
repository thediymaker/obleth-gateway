"use client";

import { useMemo, useState, useTransition, type FormEvent } from "react";
import {
  Building2,
  Check,
  ChevronDown,
  Clock3,
  RefreshCw,
  Save,
  ShieldCheck,
  UserCheck,
  UserCog,
  UserMinus,
  UserPlus,
  Users,
} from "lucide-react";
import { assignUserAction, setUserStatusAction } from "@/app/(dashboard)/users/users-actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import type { AdminUser } from "@/lib/auth/users";
import { truncateId } from "@/lib/format";
import { cn, formatNumber } from "@/lib/utils";

interface TenantOption {
  id: string;
  name: string;
}

interface Props {
  users: AdminUser[];
  tenants: TenantOption[];
}

const STATUS_STYLES: Record<AdminUser["status"], string> = {
  active: "border-emerald-500/40 bg-emerald-500/10 text-emerald-500",
  pending: "border-amber-500/40 bg-amber-500/10 text-amber-500",
};

const ROLE_STYLES: Record<AdminUser["role"], string> = {
  admin: "border-sky-500/40 bg-sky-500/10 text-sky-500",
  user: "border-border bg-background/70 text-muted-foreground",
};

function UserRow({
  user,
  tenants,
  tenantName,
}: {
  user: AdminUser;
  tenants: TenantOption[];
  tenantName?: string;
}) {
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [role, setRole] = useState<AdminUser["role"]>(user.role);
  const [tenantId, setTenantId] = useState<string>(user.tenantId ?? "");
  const isActive = user.status === "active";
  const StatusIcon = isActive ? UserCheck : Clock3;

  function handleAssign(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    setError(null);
    start(async () => {
      const result = await assignUserAction(fd);
      if (!result.ok) setError(result.error);
    });
  }

  function handleSetStatus(status: AdminUser["status"]) {
    setError(null);
    const fd = new FormData();
    fd.set("id", user.id);
    fd.set("status", status);
    start(async () => {
      const result = await setUserStatusAction(fd);
      if (!result.ok) setError(result.error);
    });
  }

  return (
    <div
      className={cn(
        "group relative overflow-hidden rounded-lg border shadow-sm transition-colors",
        isActive
          ? "border-border/70 bg-card/35 hover:border-border hover:bg-muted/15"
          : "border-amber-500/30 bg-amber-500/[0.04] ring-1 ring-amber-500/10 hover:border-amber-500/45",
      )}
    >
      <form
        onSubmit={handleAssign}
        className="grid min-w-0 md:grid-cols-[minmax(0,1.35fr)_minmax(12rem,0.7fr)_minmax(0,1.35fr)_auto] md:items-center"
      >
        <input type="hidden" name="id" value={user.id} />

        <div className="min-w-0 px-5 py-4 md:pr-5">
          <div className="flex min-w-0 items-start gap-3">
            <span
              className={cn(
                "mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md border",
                isActive
                  ? "border-emerald-500/25 bg-emerald-500/10 text-emerald-500"
                  : "border-amber-500/30 bg-amber-500/10 text-amber-500",
              )}
              aria-hidden
            >
              <StatusIcon className="h-4 w-4" />
            </span>
            <div className="min-w-0 flex-1">
              <p className="truncate font-medium" title={user.email}>
                {user.email}
              </p>
              <p className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" title={user.id}>
                ID {truncateId(user.id)}
              </p>
              <div className="mt-2 flex flex-wrap items-center gap-1.5 md:hidden">
                <UserBadges user={user} tenantName={tenantName} />
              </div>
            </div>
          </div>
        </div>

        <div className="hidden min-w-0 px-3 py-4 md:block">
          <div className="flex flex-wrap items-center gap-1.5">
            <UserBadges user={user} tenantName={tenantName} />
          </div>
        </div>

        <div className="min-w-0 px-5 pb-4 md:px-3 md:py-4">
          <div className="grid gap-2 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label
                htmlFor={`role-${user.id}`}
                className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground"
              >
                Role
              </Label>
              <ThemedSelect
                id={`role-${user.id}`}
                name="role"
                value={role}
                options={[
                  { value: "user", label: "user" },
                  { value: "admin", label: "admin" },
                ]}
                onChange={(value) => setRole(value as AdminUser["role"])}
                disabled={pending}
              />
            </div>

            <div className="space-y-1.5">
              <Label
                htmlFor={`tenant-${user.id}`}
                className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground"
              >
                Tenant
              </Label>
              <ThemedSelect
                id={`tenant-${user.id}`}
                name="tenantId"
                value={tenantId}
                options={[
                  { value: "", label: "none" },
                  ...tenants.map((tenant) => ({ value: tenant.id, label: tenant.name })),
                ]}
                onChange={setTenantId}
                disabled={pending}
                mutedValue=""
              />
            </div>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2 px-5 pb-4 md:justify-end md:px-4 md:py-4">
          <Button
            type="submit"
            size="sm"
            variant={isActive ? "secondary" : "default"}
            disabled={pending}
            aria-busy={pending}
          >
            {pending ? (
              <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
            ) : isActive ? (
              <Save className="h-3.5 w-3.5" aria-hidden />
            ) : (
              <Check className="h-3.5 w-3.5" aria-hidden />
            )}
            {pending ? "Saving" : isActive ? "Save access" : "Approve"}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={pending}
            onClick={() => handleSetStatus(isActive ? "pending" : "active")}
            className={cn(
              isActive
                ? "border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive"
                : "border-emerald-500/40 text-emerald-500 hover:bg-emerald-500/10 hover:text-emerald-500",
            )}
          >
            {isActive ? (
              <UserMinus className="h-3.5 w-3.5" aria-hidden />
            ) : (
              <UserPlus className="h-3.5 w-3.5" aria-hidden />
            )}
            {isActive ? "Deactivate" : "Reactivate"}
          </Button>
        </div>

        {error && (
          <div className="px-5 pb-4 md:col-span-4">
            <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              {error}
            </p>
          </div>
        )}
      </form>
    </div>
  );
}

export function UsersManager({ users, tenants }: Props) {
  const tenantById = useMemo(
    () => new Map(tenants.map((tenant) => [tenant.id, tenant.name])),
    [tenants],
  );
  const sorted = [...users].sort((a, b) => {
    if (a.status === "pending" && b.status !== "pending") return -1;
    if (a.status !== "pending" && b.status === "pending") return 1;
    return a.email.localeCompare(b.email);
  });

  const activeCount = users.filter((user) => user.status === "active").length;
  const pendingCount = users.length - activeCount;

  return (
    <div className="space-y-6">
      <UserStats users={users} tenants={tenants} />

      <Card>
        <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>User accounts</CardTitle>
            <CardDescription>
              {formatNumber(users.length)} registered / {formatNumber(activeCount)} active
              {pendingCount > 0 ? ` / ${formatNumber(pendingCount)} pending` : ""}
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          <div className="text-sm">
            <div className="grid border-b border-border text-left text-xs text-muted-foreground md:grid-cols-[minmax(0,1.35fr)_minmax(12rem,0.7fr)_minmax(0,1.35fr)_auto]">
              <div className="px-6 py-3 font-medium">Account</div>
              <div className="hidden px-3 py-3 font-medium md:block">Access</div>
              <div className="hidden px-3 py-3 font-medium md:block">Assignment</div>
              <div className="hidden px-3 py-3 text-right font-medium md:block" />
            </div>

            <div className="space-y-3 px-4 py-4">
              {sorted.length === 0 ? (
                <EmptyState />
              ) : (
                sorted.map((user) => (
                  <UserRow
                    key={`${user.id}-${user.role}-${user.status}-${user.tenantId ?? "none"}`}
                    user={user}
                    tenants={tenants}
                    tenantName={user.tenantId ? tenantById.get(user.tenantId) : undefined}
                  />
                ))
              )}
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function UserStats({ users, tenants }: { users: AdminUser[]; tenants: TenantOption[] }) {
  const active = users.filter((user) => user.status === "active").length;
  const pending = users.length - active;
  const admins = users.filter((user) => user.role === "admin").length;
  const linked = users.filter((user) => user.tenantId).length;

  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <StatCard
        icon={Users}
        label="Users"
        value={formatNumber(users.length)}
        hint={`${formatNumber(active)} active`}
      />
      <StatCard
        icon={Clock3}
        label="Pending approvals"
        value={formatNumber(pending)}
        hint={pending > 0 ? "waiting for review" : "none waiting"}
        tone={pending > 0 ? "warn" : undefined}
      />
      <StatCard
        icon={ShieldCheck}
        label="Admins"
        value={formatNumber(admins)}
        hint="control-plane access"
      />
      <StatCard
        icon={Building2}
        label="Tenant links"
        value={formatNumber(linked)}
        hint={`${formatNumber(tenants.length)} tenants available`}
      />
    </div>
  );
}

function UserBadges({ user, tenantName }: { user: AdminUser; tenantName?: string }) {
  return (
    <>
      <Badge className={cn("text-[10px]", STATUS_STYLES[user.status])}>{user.status}</Badge>
      <Badge className={cn("gap-1.5 text-[10px]", ROLE_STYLES[user.role])}>
        <UserCog className="h-3 w-3" aria-hidden />
        {user.role}
      </Badge>
      <Badge
        className={cn(
          "max-w-full gap-1.5 text-[10px]",
          tenantName
            ? "border-indigo-500/40 bg-indigo-500/10 text-indigo-400"
            : "border-border bg-background/70 text-muted-foreground",
        )}
        title={tenantName ?? "No tenant"}
      >
        <Building2 className="h-3 w-3 shrink-0" aria-hidden />
        <span className="truncate">{tenantName ?? "No tenant"}</span>
      </Badge>
    </>
  );
}

function ThemedSelect({
  id,
  name,
  value,
  options,
  disabled,
  mutedValue,
  onChange,
}: {
  id: string;
  name: string;
  value: string;
  options: { value: string; label: string }[];
  disabled?: boolean;
  mutedValue?: string;
  onChange: (value: string) => void;
}) {
  const selected = options.find((option) => option.value === value) ?? options[0];
  const muted = value === mutedValue;

  return (
    <>
      <input type="hidden" name={name} value={value} />
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            id={id}
            type="button"
            disabled={disabled}
            className={cn(
              "flex h-10 w-full min-w-0 items-center justify-between gap-2 rounded-md border border-border/80 bg-background/60 px-3 text-left text-sm shadow-sm transition-colors",
              "hover:border-border hover:bg-accent/50 focus:outline-none focus:ring-1 focus:ring-ring",
              "data-[state=open]:border-primary/35 data-[state=open]:bg-muted/25 data-[state=open]:ring-1 data-[state=open]:ring-primary/20",
              "disabled:cursor-not-allowed disabled:opacity-50",
            )}
          >
            <span className={cn("truncate", muted && "text-muted-foreground")}>{selected?.label ?? value}</span>
            <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground transition-transform data-[state=open]:rotate-180" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          className="max-h-72 w-[var(--radix-dropdown-menu-trigger-width)] overflow-y-auto border-border/80 bg-card/95 shadow-xl shadow-black/30"
        >
          {options.map((option) => {
            const optionSelected = option.value === value;
            const optionMuted = option.value === mutedValue;
            return (
              <DropdownMenuItem
                key={option.value || "__none__"}
                onSelect={() => onChange(option.value)}
                className={cn(
                  "cursor-pointer justify-between gap-3 rounded px-2.5 py-2",
                  optionSelected && "bg-accent text-foreground",
                )}
              >
                <span className={cn("truncate", optionMuted && "text-muted-foreground")}>{option.label}</span>
                {optionSelected && <Check className="h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />}
              </DropdownMenuItem>
            );
          })}
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
  tone,
}: {
  icon: typeof Users;
  label: string;
  value: string;
  hint: string;
  tone?: "warn";
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{label}</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
          <p className={cn("mt-0.5 text-[11px]", tone === "warn" ? "text-amber-500" : "text-muted-foreground")}>
            {hint}
          </p>
        </div>
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border/70 bg-background/40 text-muted-foreground">
          <Icon className="h-4 w-4" aria-hidden />
        </span>
      </div>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="rounded-lg border border-dashed border-border/70 px-6 py-10 text-center text-muted-foreground">
      <UserPlus className="mx-auto h-5 w-5" aria-hidden />
      <p className="mt-2 text-sm font-medium text-foreground">No users yet.</p>
      <p className="mt-1 text-xs">Approved and pending accounts will appear here.</p>
    </div>
  );
}
