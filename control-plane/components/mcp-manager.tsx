"use client";

import { useRef, useState, useTransition } from "react";
import { Trash2 } from "lucide-react";
import {
  createMcpServerAction,
  deleteMcpServerAction,
  toggleMcpServerAction,
} from "@/app/actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { McpServer } from "@/lib/obleth";

export function McpManager({ servers }: { servers: McpServer[] }) {
  const [pending, start] = useTransition();
  const [createError, setCreateError] = useState<string | null>(null);
  const createFormRef = useRef<HTMLFormElement>(null);

  function removeServer(server: McpServer) {
    if (!window.confirm(`Remove MCP server "${server.name}"? This cannot be undone.`)) return;
    start(() => deleteMcpServerAction(server.id));
  }

  function submitServer(formData: FormData) {
    setCreateError(null);
    start(async () => {
      const result = await createMcpServerAction(formData);
      if (result.ok) {
        createFormRef.current?.reset();
      } else {
        setCreateError(result.error);
      }
    });
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Register MCP server</CardTitle>
          <CardDescription>
            Front an MCP (Model Context Protocol) server behind obleth. Clients reach it at{" "}
            <code className="rounded bg-secondary px-1 py-0.5 text-xs">/mcp/&#123;name&#125;</code>{" "}
            using their obleth API key; obleth injects the upstream credential and audits access.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form
            ref={createFormRef}
            action={submitServer}
            className="grid gap-4 md:grid-cols-2"
          >
            <Field label="Name (path segment)" name="name" placeholder="github" required />
            <Field
              label="Upstream URL"
              name="upstream_url"
              type="url"
              placeholder="https://mcp.example.com/sse"
              required
            />
            <div className="md:col-span-2">
              <Field
                label="Auth header (optional)"
                name="auth_header"
                placeholder="Bearer sk-…"
              />
            </div>
            <div className="md:col-span-2">
              <Button type="submit" disabled={pending}>
                {pending ? "Adding…" : "Register server"}
              </Button>
            </div>
            {createError && (
              <p className="md:col-span-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {createError}
              </p>
            )}
          </form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Registered MCP servers</CardTitle>
          <CardDescription>
            {servers.length} server{servers.length === 1 ? "" : "s"} registered
          </CardDescription>
        </CardHeader>
        <CardContent className="overflow-x-auto p-0">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-6 py-3 font-medium">Name</th>
                <th className="px-3 py-3 font-medium">Endpoint</th>
                <th className="px-3 py-3 font-medium">Upstream</th>
                <th className="px-3 py-3 font-medium">Auth</th>
                <th className="px-3 py-3 font-medium">Status</th>
                <th className="px-3 py-3 font-medium" />
              </tr>
            </thead>
            <tbody>
              {servers.map((s) => (
                <tr key={s.id} className="border-b border-border/60">
                  <td className="px-6 py-3 font-medium">{s.name}</td>
                  <td className="px-3 py-3 font-mono text-xs text-muted-foreground">/mcp/{s.name}</td>
                  <td className="max-w-[220px] truncate px-3 py-3 font-mono text-xs text-muted-foreground">
                    {s.upstream_url}
                  </td>
                  <td className="px-3 py-3 text-xs text-muted-foreground">
                    {s.auth_header ? "set" : "none"}
                  </td>
                  <td className="px-3 py-3">
                    <button
                      type="button"
                      disabled={pending}
                      onClick={() =>
                        start(() => toggleMcpServerAction(s.id, s.upstream_url, !s.enabled))
                      }
                      aria-label={`${s.enabled ? "Disable" : "Enable"} ${s.name}`}
                      title="Click to toggle"
                    >
                      <Badge className={s.enabled ? "text-foreground" : "opacity-50"}>
                        {s.enabled ? "enabled" : "disabled"}
                      </Badge>
                    </button>
                  </td>
                  <td className="px-3 py-3">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-destructive hover:text-destructive"
                      disabled={pending}
                      onClick={() => removeServer(s)}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      Remove
                    </Button>
                  </td>
                </tr>
              ))}
              {servers.length === 0 && (
                <tr>
                  <td colSpan={6} className="px-6 py-8 text-center text-muted-foreground">
                    No MCP servers registered.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </CardContent>
      </Card>
    </div>
  );
}

function Field({
  label,
  name,
  placeholder,
  required,
  type = "text",
}: {
  label: string;
  name: string;
  placeholder?: string;
  required?: boolean;
  type?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={name}>{label}</Label>
      <Input id={name} name={name} type={type} placeholder={placeholder} required={required} />
    </div>
  );
}
