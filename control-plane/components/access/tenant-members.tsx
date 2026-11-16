"use client";

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { Plus } from "lucide-react";
import { addTenantMember, removeTenantMember } from "@/app/(dashboard)/tenants/[name]/members-actions";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Select } from "@/components/ui/select";
import type { Member } from "@/lib/obleth-accounts";

const ROLES = [{ value: "member", label: "member" }, { value: "manager", label: "manager" }];

/** Members of one tenant: add by email, change nothing for the owner of a personal tenant. */
export function TenantMembers({ tenantId, tenantName, members, users }: { tenantId: string; tenantName: string; members: Member[]; users: string[] }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [email, setEmail] = useState("");
  const personal = members.some((m) => m.role === "owner");
  const taken = new Set(members.map((m) => m.email?.toLowerCase()));
  const candidates = users.filter((u) => !taken.has(u)).map((u) => ({ value: u, label: u }));

  function remove(m: Member) {
    start(async () => {
      const who = m.email ?? m.subject;
      const ok = await confirm({ title: `Remove ${who} from ${tenantName}?`, description: "Their keys in this tenant are deleted.", confirmLabel: "Remove" });
      if (!ok) return;
      const fd = new FormData();
      fd.set("tenantId", tenantId);
      fd.set("issuer", m.issuer);
      fd.set("subject", m.subject);
      const res = await removeTenantMember(fd);
      if (res.ok) { setError(null); router.refresh(); } else setError(res.error);
    });
  }

  return (
    <div className="flex flex-col gap-3">
      {confirmElement}
      {!personal && (
        <form
          className="flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            const form = e.currentTarget;
            const fd = new FormData(form);
            start(async () => {
              const res = await addTenantMember(fd);
              if (res.ok) { setError(null); setEmail(""); form.reset(); router.refresh(); } else setError(res.error);
            });
          }}
        >
          <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Add a member</span>
          <fieldset disabled={pending} className="flex flex-wrap items-center gap-2">
            <input type="hidden" name="tenantId" value={tenantId} />
            <Select name="email" required searchable value={email} onValueChange={setEmail} aria-label="Member email" placeholder={candidates.length ? "Choose a user" : "Everyone is already a member"} searchPlaceholder="Find by email" className="min-w-[200px] flex-1" options={candidates} />
            <Select name="role" defaultValue="member" aria-label="Role" className="w-32" options={ROLES} />
            <Button type="submit" size="sm" className="h-9" disabled={!email}><Plus className="h-3.5 w-3.5" aria-hidden />Add</Button>
          </fieldset>
        </form>
      )}

      {error && <div role="alert" className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>}

      <div aria-label="Members">
        {members.length === 0 && <p className="py-4 text-center text-[13px] text-muted-foreground">No members yet.</p>}
        {members.map((m) => (
          <div key={`${m.issuer}|${m.subject}`} className="grid min-h-[40px] grid-cols-[minmax(0,1fr)_88px_88px] items-center gap-3 border-t border-border py-1 text-[13px]">
            <span className="truncate">{m.email ?? <span className="font-mono text-[12px]">{m.subject}</span>}</span>
            <span><Pill>{m.role}</Pill></span>
            <span className="text-right">
              {m.role !== "owner" && <Button type="button" variant="ghost" size="sm" disabled={pending} onClick={() => remove(m)}>Remove</Button>}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
