"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useMemo, useState } from "react";
import {
  BarChart3,
  Boxes,
  ChevronDown,
  ExternalLink,
  KeyRound,
  LogOut,
  PanelLeftClose,
  PanelLeftOpen,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { authClient } from "@/lib/auth/client";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { OblethLogo } from "@/components/obleth-logo";
import { StatusFooter } from "@/components/status-footer";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";

const SIDEBAR_STORAGE_KEY = "obleth-portal-sidebar-collapsed";

const nav = [
  { href: "/portal/models", label: "Models", icon: Boxes },
  { href: "/portal/keys", label: "Keys", icon: KeyRound },
  { href: "/portal/usage", label: "Usage", icon: BarChart3 },
];

function userInitials(name: string) {
  const parts = name.trim().split(/[\s._-]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

function NavItem({
  href,
  label,
  icon: Icon,
  active,
  collapsed,
}: {
  href: string;
  label: string;
  icon: LucideIcon;
  active: boolean;
  collapsed: boolean;
}) {
  const link = (
    <Link
      href={href}
      className={cn(
        "flex items-center rounded-md py-2 text-sm transition-colors",
        collapsed ? "justify-center px-2" : "gap-2.5 px-3",
        active
          ? "bg-secondary text-foreground"
          : "text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      <Icon className="h-4 w-4 shrink-0 opacity-80" aria-hidden />
      {!collapsed && <span className="truncate">{label}</span>}
    </Link>
  );

  if (!collapsed) return link;

  return (
    <Tooltip delayDuration={0}>
      <TooltipTrigger asChild>{link}</TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}

export function PortalShell({
  children,
  username,
  tenantName,
  role,
  version,
}: {
  children: React.ReactNode;
  username: string;
  tenantName: string;
  role: "admin" | "user";
  version: string;
}) {
  const pathname = usePathname();
  const router = useRouter();
  const [collapsed, setCollapsed] = useState(false);
  const [hydrated, setHydrated] = useState(false);

  useEffect(() => {
    const stored = localStorage.getItem(SIDEBAR_STORAGE_KEY);
    if (stored !== null) {
      setCollapsed(stored === "true");
    } else {
      setCollapsed(window.matchMedia("(max-width: 767px)").matches);
    }
    setHydrated(true);
  }, []);

  function toggleSidebar() {
    setCollapsed((prev) => {
      const next = !prev;
      localStorage.setItem(SIDEBAR_STORAGE_KEY, String(next));
      return next;
    });
  }

  async function logout() {
    await authClient.signOut();
    router.push("/login");
    router.refresh();
  }

  const pageTitle = useMemo(() => {
    const match = nav.find(({ href }) => pathname.startsWith(href));
    return match?.label ?? "Portal";
  }, [pathname]);

  const initials = userInitials(username);
  const sidebarCollapsed = hydrated && collapsed;

  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-screen overflow-hidden bg-background">
        <aside
          className={cn(
            "hidden h-full shrink-0 flex-col border-r border-border bg-card/40 transition-[width] duration-200 ease-in-out sm:flex",
            sidebarCollapsed ? "w-14" : "w-56",
          )}
        >
          <div
            className={cn(
              "flex h-14 shrink-0 items-center border-b border-border",
              sidebarCollapsed ? "justify-center px-2" : "gap-2.5 px-4",
            )}
          >
            <OblethLogo size={28} />
            {!sidebarCollapsed && (
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-semibold tracking-tight">obleth</div>
                <div className="truncate text-[10px] uppercase tracking-wider text-muted-foreground">
                  User Portal
                </div>
              </div>
            )}
          </div>
          <nav className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2">
            {nav.map(({ href, label, icon }) => (
              <NavItem
                key={href}
                href={href}
                label={label}
                icon={icon}
                active={pathname.startsWith(href)}
                collapsed={sidebarCollapsed}
              />
            ))}
          </nav>
          {!sidebarCollapsed && (
            <div className="border-t border-border p-3">
              <p className="truncate text-[10px] uppercase tracking-wider text-muted-foreground">
                Tenant
              </p>
              <p className="mt-1 truncate text-sm font-medium" title={tenantName}>
                {tenantName}
              </p>
            </div>
          )}
        </aside>

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <header className="flex h-14 shrink-0 items-center justify-between gap-3 border-b border-border px-3 sm:px-6">
            <div className="flex min-w-0 items-center gap-2">
              <Button
                variant="ghost"
                size="icon"
                className="hidden shrink-0 sm:inline-flex"
                onClick={toggleSidebar}
                aria-label={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"}
              >
                {sidebarCollapsed ? (
                  <PanelLeftOpen className="h-4 w-4" aria-hidden />
                ) : (
                  <PanelLeftClose className="h-4 w-4" aria-hidden />
                )}
              </Button>
              <div className="flex items-center gap-2 sm:hidden">
                <OblethLogo size={24} />
              </div>
              <div className="min-w-0">
                <h1 className="truncate text-sm font-medium">{pageTitle}</h1>
                <p className="truncate text-[11px] text-muted-foreground sm:hidden">{tenantName}</p>
              </div>
            </div>

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" className="h-9 shrink-0 gap-2 px-2 sm:px-3">
                  <div className="flex h-7 w-7 items-center justify-center rounded-full bg-secondary text-xs font-medium">
                    {initials}
                  </div>
                  <span className="hidden max-w-[12rem] truncate md:inline">{username}</span>
                  <ChevronDown className="h-4 w-4 shrink-0 opacity-50" aria-hidden />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-64">
                <DropdownMenuLabel className="font-normal">
                  <div className="flex flex-col gap-0.5">
                    <span className="truncate font-medium">{username}</span>
                    <span className="truncate text-xs font-normal text-muted-foreground">
                      {tenantName}
                    </span>
                  </div>
                </DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuItem disabled className="text-xs text-muted-foreground">
                  Portal v{version}
                </DropdownMenuItem>
                {role === "admin" && (
                  <>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem asChild>
                      <Link href="/">
                        <ExternalLink className="mr-2 h-4 w-4" aria-hidden />
                        Admin dashboard
                      </Link>
                    </DropdownMenuItem>
                  </>
                )}
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={logout} className="text-destructive focus:text-destructive">
                  <LogOut className="mr-2 h-4 w-4" aria-hidden />
                  Sign out
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </header>

          <nav className="grid grid-cols-3 gap-1 border-b border-border bg-card/35 p-2 sm:hidden">
            {nav.map(({ href, label, icon: Icon }) => {
              const active = pathname.startsWith(href);
              return (
                <Link
                  key={href}
                  href={href}
                  className={cn(
                    "inline-flex items-center justify-center gap-1.5 rounded-md px-2 py-2 text-xs font-medium transition-colors",
                    active ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-accent",
                  )}
                >
                  <Icon className="h-3.5 w-3.5" aria-hidden />
                  {label}
                </Link>
              );
            })}
          </nav>

          <main className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-6 lg:p-8">{children}</main>
          <StatusFooter />
        </div>
      </div>
    </TooltipProvider>
  );
}
