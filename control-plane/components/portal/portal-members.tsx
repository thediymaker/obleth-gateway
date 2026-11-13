"use client";

import { useState } from "react";
import { Plus } from "lucide-react";
import type { Member } from "@/lib/obleth-accounts";
import { addAction, removeAction } from "@/app/(portal)/portal/members/members-actions";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";

const ROLES = [{ value: "member", label: "member" }, { value: "manager", label: "manager" }];
const COLS = "grid-cols-[minmax(0,1fr)_88px_88px]";

export function PortalMembers({
  members,
  me,
  users,
  error,
  ok,
}: {
  members: Member[];
  me: { issuer: string; subject: string };
  users: string[];
  error?: string;
  ok?: string;
}) {
  const [email, setEmail] = useState("");
  const taken = new Set(members.map((m) => m.email?.toLowerCase()));
  const candidates = users.filter((u) => !taken.has(u)).map((u) => ({ value: u, label: u }));

  return (
    <div className="flex max-w-[760px] flex-col gap-5">
      <div className="flex flex-col gap-1">
        <h1 className="text-lg font-semibold tracking-tight">Members</h1>
        <p className="text-sm text-muted-foreground">{members.length} member{members.length === 1 ? "" : "s"}</p>
      </div>

      <form action={addAction} className="flex flex-col gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Add a member</span>
        <div className="flex flex-wrap items-center gap-2">
          <Select
            name="email"
            required
            searchable
            value={email}
            onValueChange={setEmail}
            aria-label="Member email"
            placeholder={candidates.length ? "Choose a user" : "Everyone is already a member"}
            searchPlaceholder="Find by email"
            className="min-w-[200px] flex-1"
            options={candidates}
          />
          <Select name="role" defaultValue="member" aria-label="Role" className="w-32" options={ROLES} />
          <Button type="submit" size="sm" className="h-9" disabled={!email}><Plus className="h-3.5 w-3.5" aria-hidden />Add</Button>
        </div>
      </form>

      {error && <div role="alert" className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</div>}
      {!error && ok && <p role="status" className="text-sm text-green-700">{ok}</p>}

      <section aria-label="Members" className="overflow-hidden rounded-lg border border-border">
        <div className={`grid ${COLS} items-center gap-3 px-4 py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground`}>
          <span>Member</span><span>Role</span><span />
        </div>
        {members.length === 0 && <p className="border-t border-border px-4 py-6 text-center text-[13px] text-muted-foreground">No members yet.</p>}
        {members.map((m) => (
          <div key={`${m.issuer}|${m.subject}`} className={`grid min-h-[44px] ${COLS} items-center gap-3 border-t border-border px-4 py-1.5 text-[13px]`}>
            <span className="truncate">
              {m.email ?? <span className="font-mono text-[12px]">{m.subject}</span>}
              {m.issuer === me.issuer && m.subject === me.subject && <span className="text-muted-foreground"> (you)</span>}
            </span>
            <span><Pill>{m.role}</Pill></span>
            <span className="text-right">
              {m.role !== "owner" && (
                <form action={removeAction}>
                  <input type="hidden" name="issuer" value={m.issuer} />
                  <input type="hidden" name="subject" value={m.subject} />
                  <Button type="submit" variant="ghost" size="sm">Remove</Button>
                </form>
              )}
            </span>
          </div>
        ))}
      </section>
    </div>
  );
}
