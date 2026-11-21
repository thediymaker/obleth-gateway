"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { createTenantAction } from "@/app/actions";
import { Field } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { tenantHref } from "@/lib/access-model";
import type { Tenant } from "@/lib/obleth";
import { Sheet } from "@/components/ui/sheet";

/** The fairshare group a tenant starts in unless it is given another. */
export const DEFAULT_FAIRSHARE_GROUP = "default";

/**
 * Add a tenant: its name, organization, group, weight and caps, with the
 * labels and defaults of its settings page. Hours, budget, models and
 * policies are set on that page, which opens once it is made.
 */
export function AddTenantSheet({ open, onClose, tenants }: { open: boolean; onClose: () => void; tenants: Tenant[] }) {
  const router = useRouter();
  const [group, setGroup] = useState(DEFAULT_FAIRSHARE_GROUP);
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const groups = [...new Set(tenants.map((t) => t.fairshare_group))].sort();
  const inGroup = tenants.filter((t) => t.fairshare_group === group).length;
  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="Add a tenant"
      description="A team or project with its own keys and share of the gateway. Its hours, budget, models and policies are on its page, which opens next."
      width="w-[min(520px,100vw)]"
      footer={
        <div className="flex items-center justify-between gap-3">
          <span role="alert" className="text-[12.5px] font-medium">{error}</span>
          <div className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onClose} disabled={pending}>Cancel</Button>
            <Button type="submit" form="add-tenant-form" size="sm" disabled={pending}>{pending ? "Adding…" : "Add tenant"}</Button>
          </div>
        </div>
      }
    >
      <form
        id="add-tenant-form"
        className="flex flex-col gap-4 px-6 pb-6"
        onSubmit={(e) => {
          e.preventDefault();
          const fd = new FormData(e.currentTarget);
          setError(null);
          start(async () => {
            const res = await createTenantAction(fd);
            if (!res.ok) return setError(res.error);
            onClose();
            router.push(tenantHref(res.tenant));
          });
        }}
      >
        <Field label="Name" name="name" required placeholder="research-computing" hint="Shown everywhere, and in its page's address." />
        <Field label="Organization" name="organization" placeholder="Department or group" />
        <Field
          label="Fairshare group"
          name="fairshare_group"
          required
          maxLength={64}
          list="add-tenant-groups"
          value={group}
          onChange={(e) => setGroup(e.target.value.trim() || e.target.value)}
          className="font-mono"
          hint={groups.includes(group) ? `Tenants in a group share its slots by weight. ${inGroup} tenant${inGroup === 1 ? "" : "s"} here; type a new name to start a group.` : "A new group, made at weight 100."}
        />
        <datalist id="add-tenant-groups">{groups.map((g) => <option key={g} value={g} />)}</datalist>
        <Field label="Weight" name="weight" type="number" min={1} step={1} required defaultValue="100" hint="Its share against the other tenants in the group." />
        <div className="grid grid-cols-2 gap-3">
          <Field label="Token rate" name="tokens_per_minute" type="number" min={0} step={1} placeholder="No limit" hint="Tokens a minute across all its keys." />
          <Field label="In flight per model" name="max_in_flight" type="number" min={1} step={1} placeholder="No cap" hint="Requests at once on any one model." />
        </div>
      </form>
    </Sheet>
  );
}
