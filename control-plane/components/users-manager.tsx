"use client";

import { useState, useTransition } from "react";
import { assignUserAction, setUserStatusAction } from "@/app/(dashboard)/users/users-actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import type { AdminUser } from "@/lib/auth/users";

interface TenantOption {
  id: string;
  name: string;
}

interface Props {
  users: AdminUser[];
  tenants: TenantOption[];
}

const STATUS_STYLES: Record<string, string> = {
  active: "border-emerald-500/40 bg-emerald-500/10 text-emerald-500",
  pending: "border-amber-500/40 bg-amber-500/10 text-amber-500",
};

function UserRow({
  user,
  tenants,
}: {
  user: AdminUser;
  tenants: TenantOption[];
}) {
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [role, setRole] = useState<string>(user.role);
  const [tenantId, setTenantId] = useState<string>(user.tenantId ?? "");

  function handleAssign(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    setError(null);
    start(async () => {
      const result = await assignUserAction(fd);
      if (!result.ok) setError(result.error);
    });
  }

  function handleSetStatus(status: "active" | "pending") {
    setError(null);
    const fd = new FormData();
    fd.set("id", user.id);
    fd.set("status", status);
    start(async () => {
      const result = await setUserStatusAction(fd);
      if (!result.ok) setError(result.error);
    });
  }

  const isActive = user.status === "active";

  return (
    <div className="grid grid-cols-1 gap-3 border-b border-border/60 px-5 py-4 last:border-b-0 sm:grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto]">
      {/* Identity */}
      <div className="flex min-w-0 flex-col justify-center gap-1">
        <p className="truncate text-sm font-medium" title={user.email}>
          {user.email}
        </p>
        <div className="flex items-center gap-2">
          <Badge className={STATUS_STYLES[user.status] ?? "border-border bg-muted/30 text-muted-foreground"}>
            {user.status}
          </Badge>
          <span className="text-xs text-muted-foreground font-mono truncate" title={user.id}>
            {user.id.slice(0, 8)}…
          </span>
        </div>
      </div>

      {/* Assignment form */}
      <form onSubmit={handleAssign} className="flex min-w-0 flex-wrap items-end gap-2">
        <input type="hidden" name="id" value={user.id} />

        <div className="flex flex-col gap-1">
          <Label htmlFor={`role-${user.id}`} className="text-xs">
            Role
          </Label>
          <select
            id={`role-${user.id}`}
            name="role"
            value={role}
            onChange={(e) => setRole(e.target.value)}
            disabled={pending}
            className="h-9 rounded-md border border-input bg-background px-2 py-1 text-sm shadow-sm focus:outline-none focus:ring-1 focus:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value="user">user</option>
            <option value="admin">admin</option>
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <Label htmlFor={`tenant-${user.id}`} className="text-xs">
            Tenant
          </Label>
          <select
            id={`tenant-${user.id}`}
            name="tenantId"
            value={tenantId}
            onChange={(e) => setTenantId(e.target.value)}
            disabled={pending}
            className="h-9 rounded-md border border-input bg-background px-2 py-1 text-sm shadow-sm focus:outline-none focus:ring-1 focus:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value="">— none —</option>
            {tenants.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </div>

        <Button type="submit" size="sm" disabled={pending} className="shrink-0">
          {pending ? "Saving…" : "Assign / Approve"}
        </Button>
      </form>

      {/* Deactivate / reactivate */}
      <div className="flex items-end">
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pending}
          onClick={() => handleSetStatus(isActive ? "pending" : "active")}
          className={
            isActive
              ? "border-destructive/40 text-destructive hover:bg-destructive/10"
              : "border-emerald-500/40 text-emerald-600 hover:bg-emerald-500/10"
          }
        >
          {isActive ? "Deactivate" : "Reactivate"}
        </Button>
      </div>

      {error && (
        <p className="col-span-full rounded-md border border-destructive/40 bg-destructive/10 px-3 py-1.5 text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

export function UsersManager({ users, tenants }: Props) {
  // Pending users sort to the top
  const sorted = [...users].sort((a, b) => {
    if (a.status === "pending" && b.status !== "pending") return -1;
    if (a.status !== "pending" && b.status === "pending") return 1;
    return 0;
  });

  const pendingCount = users.filter((u) => u.status === "pending").length;

  return (
    <Card>
      <CardHeader>
        <div className="flex items-center justify-between gap-2">
          <div>
            <CardTitle>User accounts</CardTitle>
            <CardDescription>
              {users.length} user{users.length !== 1 ? "s" : ""}
              {pendingCount > 0 ? ` · ${pendingCount} pending approval` : ""}
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {/* Column headers */}
        <div className="hidden grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto] border-b border-border px-5 py-2 text-xs font-medium uppercase tracking-wider text-muted-foreground sm:grid">
          <div>Account</div>
          <div>Assignment</div>
          <div />
        </div>

        {sorted.length === 0 ? (
          <div className="px-5 py-10 text-center text-sm text-muted-foreground">
            No users yet.
          </div>
        ) : (
          sorted.map((user) => (
            <UserRow key={user.id} user={user} tenants={tenants} />
          ))
        )}
      </CardContent>
    </Card>
  );
}
