"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { authClient } from "@/lib/auth/client";
import { useEffect, useMemo, useState } from "react";
import {
  BookText,
  Boxes,
  BarChart3,
  Bot,
  ChevronDown,
  Gauge,
  KeyRound,
  LayoutDashboard,
  LogOut,
  PanelLeftClose,
  PanelLeftOpen,
  Plug,
  Radio,
  ScrollText,
  Settings,
  UserCog,
  Users,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { OblethLogo } from "@/components/obleth-logo";
import { StatusFooter } from "@/components/status-footer";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";

const SIDEBAR_STORAGE_KEY = "obleth-sidebar-collapsed";

const nav = [
  { href: "/", label: "Overview", icon: LayoutDashboard },
  { href: "/fairshare", label: "Fairshare", icon: Gauge },
  { href: "/models", label: "Models", icon: Boxes },
  { href: "/recipes", label: "Recipes", icon: BookText },
  { href: "/users", label: "Users", icon: UserCog },
  { href: "/mcp", label: "MCP Servers", icon: Plug },
  { href: "/tenants", label: "Tenants", icon: Users },
  { href: "/keys", label: "API Keys", icon: KeyRound },
  { href: "/logs", label: "Request Logs", icon: Radio },
  { href: "/reports", label: "Reports", icon: BarChart3 },
  { href: "/audit", label: "Audit", icon: ScrollText },
  { href: "/charo", label: "Charo", icon: Bot },
  { href: "/settings", label: "Settings", icon: Settings },
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
      <Icon className="h-4 w-4 shrink-0 opacity-80" />
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

export function AppShell({
  children,
  username,
  version,
}: {
  children: React.ReactNode;
  username: string;
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
    const match = nav.find(({ href }) => (href === "/" ? pathname === "/" : pathname.startsWith(href)));
    return match?.label ?? "Dashboard";
  }, [pathname]);

  const initials = userInitials(username);
  const sidebarCollapsed = hydrated && collapsed;

  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-screen overflow-hidden bg-background">
        <aside
          className={cn(
            "flex h-full shrink-0 flex-col border-r border-border bg-card/40 transition-[width] duration-200 ease-in-out",
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
                  Control Plane
                </div>
              </div>
            )}
          </div>
          <nav className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto p-2">
            {nav.map(({ href, label, icon }) => {
              const active = href === "/" ? pathname === "/" : pathname.startsWith(href);
              return (
                <NavItem
                  key={href}
                  href={href}
                  label={label}
                  icon={icon}
                  active={active}
                  collapsed={sidebarCollapsed}
                />
              );
            })}
          </nav>
        </aside>

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <header className="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-border px-4 sm:px-6">
            <div className="flex min-w-0 items-center gap-2">
              <Button
                variant="ghost"
                size="icon"
                className="shrink-0"
                onClick={toggleSidebar}
                aria-label={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"}
              >
                {sidebarCollapsed ? (
                  <PanelLeftOpen className="h-4 w-4" />
                ) : (
                  <PanelLeftClose className="h-4 w-4" />
                )}
              </Button>
              <h1 className="truncate text-sm font-medium">{pageTitle}</h1>
            </div>

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" className="h-9 shrink-0 gap-2 px-2 sm:px-3">
                  <div className="flex h-7 w-7 items-center justify-center rounded-full bg-secondary text-xs font-medium">
                    {initials}
                  </div>
                  <span className="hidden max-w-[10rem] truncate sm:inline">{username}</span>
                  <ChevronDown className="h-4 w-4 shrink-0 opacity-50" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-56">
                <DropdownMenuLabel className="font-normal">
                  <div className="flex flex-col gap-0.5">
                    <span className="truncate font-medium">{username}</span>
                    <span className="text-xs font-normal text-muted-foreground">Administrator</span>
                  </div>
                </DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuItem disabled className="text-xs text-muted-foreground">
                  Control plane v{version}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={logout} className="text-destructive focus:text-destructive">
                  <LogOut className="mr-2 h-4 w-4" />
                  Sign out
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </header>
          <main className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-6 lg:p-8">{children}</main>
          <StatusFooter />
        </div>
      </div>
    </TooltipProvider>
  );
}
