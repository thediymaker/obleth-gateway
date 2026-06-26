"use client";

import { useState, useTransition } from "react";
import { Check, Copy, KeyRound, Plus, Trash2 } from "lucide-react";
import {
  createPortalKey,
  deletePortalKey,
  disablePortalKey,
} from "@/app/(portal)/portal-actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { ApiKey } from "@/lib/obleth";

export function PortalKeys({ keys }: { keys: ApiKey[] }) {
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});
  const [pending, start] = useTransition();

  async function copySecret() {
    if (!secret) return;
    await navigator.clipboard.writeText(secret);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 2000);
  }

  function handleCreate(formData: FormData) {
    setCreateError(null);
    start(async () => {
      const result = await createPortalKey(formData);
      if (result.ok) {
        setSecret(result.secret);
        setCopied(false);
        setCreateOpen(false);
      } else {
        setCreateError(result.error);
      }
    });
  }

  function handleToggle(key: ApiKey) {
    setRowErrors((prev) => ({ ...prev, [key.id]: "" }));
    start(async () => {
      const fd = new FormData();
      fd.set("id", key.id);
      fd.set("disabled", String(!key.disabled));
      const result = await disablePortalKey(fd);
      if (!result.ok) {
        setRowErrors((prev) => ({ ...prev, [key.id]: result.error }));
      }
    });
  }

  function handleDelete(key: ApiKey) {
    if (!window.confirm(`Delete API key "${key.name}"? This cannot be undone.`))
      return;
    setRowErrors((prev) => ({ ...prev, [key.id]: "" }));
    start(async () => {
      const fd = new FormData();
      fd.set("id", key.id);
      const result = await deletePortalKey(fd);
      if (!result.ok) {
        setRowErrors((prev) => ({ ...prev, [key.id]: result.error }));
      }
    });
  }

  return (
    <div className="space-y-4">
      {/* Secret reveal banner shown once after key creation */}
      {secret && (
        <Card className="border-foreground/25">
          <CardContent className="pt-6">
            <p className="mb-1 text-sm font-semibold">Your new API key</p>
            <p className="mb-2 text-sm text-muted-foreground">
              Copy this key now — it will not be shown again.
            </p>
            <code className="block break-all rounded-md border border-border bg-background px-3 py-2 font-mono text-xs">
              {secret}
            </code>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Button variant="secondary" size="sm" onClick={copySecret}>
                {copied ? (
                  <Check className="h-3.5 w-3.5" />
                ) : (
                  <Copy className="h-3.5 w-3.5" />
                )}
                {copied ? "Copied" : "Copy key"}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setSecret(null)}
              >
                Dismiss
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader className="flex flex-row items-center justify-between gap-4">
          <div>
            <CardTitle>API keys</CardTitle>
            <CardDescription>
              Keys scoped to your tenant. Use them to authenticate requests to
              the gateway.
            </CardDescription>
          </div>

          <Dialog open={createOpen} onOpenChange={setCreateOpen}>
            <DialogTrigger asChild>
              <Button size="sm">
                <Plus className="h-4 w-4" />
                New key
              </Button>
            </DialogTrigger>
            <DialogContent className="max-w-md">
              <DialogHeader>
                <DialogTitle>Create API key</DialogTitle>
                <DialogDescription>
                  Give the key a memorable name. You can create multiple keys
                  for different workloads.
                </DialogDescription>
              </DialogHeader>
              <form action={handleCreate} className="space-y-4">
                <div className="space-y-1.5">
                  <Label htmlFor="new-key-name">Key name</Label>
                  <Input
                    id="new-key-name"
                    name="name"
                    placeholder="prod-chat"
                    required
                  />
                </div>
                {createError && (
                  <p className="rounded-md border border-destructive/40 px-3 py-2 text-sm text-destructive">
                    {createError}
                  </p>
                )}
                <DialogFooter>
                  <Button
                    type="button"
                    variant="ghost"
                    onClick={() => setCreateOpen(false)}
                    disabled={pending}
                  >
                    Cancel
                  </Button>
                  <Button
                    type="submit"
                    disabled={pending}
                    className="border border-foreground/25"
                  >
                    <KeyRound className="h-4 w-4" />
                    {pending ? "Creating..." : "Create key"}
                  </Button>
                </DialogFooter>
              </form>
            </DialogContent>
          </Dialog>
        </CardHeader>

        <CardContent className="p-0">
          {keys.length === 0 ? (
            <div className="px-6 py-10 text-center text-sm text-muted-foreground">
              No keys yet. Create your first key above.
            </div>
          ) : (
            <ul className="divide-y">
              {keys.map((key) => (
                <li key={key.id} className="flex items-center gap-3 px-6 py-4">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-medium">{key.name}</span>
                      <Badge
                        className={
                          key.disabled
                            ? "opacity-50"
                            : "border-emerald-500/40 text-emerald-500"
                        }
                      >
                        {key.disabled ? "disabled" : "active"}
                      </Badge>
                    </div>
                    <div className="mt-0.5 font-mono text-xs text-muted-foreground">
                      {key.key_prefix}...
                    </div>
                    {rowErrors[key.id] && (
                      <p className="mt-1 text-xs text-destructive">
                        {rowErrors[key.id]}
                      </p>
                    )}
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={pending}
                      onClick={() => handleToggle(key)}
                    >
                      {key.disabled ? "Enable" : "Disable"}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-8 w-8 text-destructive"
                      disabled={pending}
                      onClick={() => handleDelete(key)}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      <span className="sr-only">Delete {key.name}</span>
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
