"use client";

import type { ReactNode } from "react";
import {
  Bell,
  Route,
  Database,
  Info,
  Server,
  Bot,
  Archive,
  BookOpen,
  Sparkles,
  Zap,
} from "lucide-react";
import {
  AlertSettingsForm,
  AutoRouterSettingsForm,
  BoonsSettingsForm,
  CharoSettingsForm,
  CompressionSettingsForm,
  SlurmSettingsForm,
  ResolverCacheCard,
  UsageRetentionForm,
} from "@/components/settings-form";
import { EnergySettingsForm } from "@/components/energy-settings-form";
import { BackupRestore } from "@/components/backup-restore";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  EnergySettingsView,
  KnowledgeSettingsView,
  ModelRoute,
  SlurmSettingsView,
  UsageRetentionView,
  RouterReadinessView,
} from "@/lib/obleth";
import { RouterReadinessCard } from "@/components/router-readiness-card";

export function SettingsTabs({
  alertSettings,
  autoRouter,
  boons,
  charo,
  compressor,
  energy,
  knowledge,
  models,
  retention,
  routerReadiness,
  slurm,
  versionCard,
  initialTab = "alerts",
}: {
  alertSettings: AlertSettingsView | null;
  autoRouter: AutoRouterSettingsView | null;
  boons: BoonSettingsView | null;
  charo: CharoSettingsView | null;
  compressor: CompressorStatusView | null;
  energy: EnergySettingsView | null;
  knowledge: KnowledgeSettingsView | null;
  models: ModelRoute[];
  retention: UsageRetentionView | null;
  routerReadiness: RouterReadinessView | null;
  slurm: SlurmSettingsView | null;
  versionCard: ReactNode;
  /** The tab to open on, from `?tab=` (e.g. Deployments links to `slurm`). */
  initialTab?: string;
}) {
  return (
    <Tabs defaultValue={initialTab}>
      <TabsList>
        <TabsTrigger value="alerts">
          <Bell className="h-3.5 w-3.5" />
          Alerts
        </TabsTrigger>
        <TabsTrigger value="routing">
          <Route className="h-3.5 w-3.5" />
          Routing
        </TabsTrigger>
        <TabsTrigger value="boons">
          <Sparkles className="h-3.5 w-3.5" />
          Boons
        </TabsTrigger>
        <TabsTrigger value="compression">
          <Archive className="h-3.5 w-3.5" />
          Compression
        </TabsTrigger>
        <TabsTrigger value="knowledge">
          <BookOpen className="h-3.5 w-3.5" />
          Knowledge
        </TabsTrigger>
        <TabsTrigger value="energy">
          <Zap className="h-3.5 w-3.5" />
          Energy
        </TabsTrigger>
        <TabsTrigger value="data">
          <Database className="h-3.5 w-3.5" />
          Data
        </TabsTrigger>
        <TabsTrigger value="slurm">
          <Server className="h-3.5 w-3.5" />
          Slurm
        </TabsTrigger>
        <TabsTrigger value="assistant">
          <Bot className="h-3.5 w-3.5" />
          Assistant
        </TabsTrigger>
        <TabsTrigger value="about">
          <Info className="h-3.5 w-3.5" />
          About
        </TabsTrigger>
      </TabsList>

      <TabsContent value="alerts">
        <AlertSettingsForm settings={alertSettings} />
      </TabsContent>

      <TabsContent value="routing">
        <div className="space-y-6">
          <RouterReadinessCard readiness={routerReadiness} />
          <AutoRouterSettingsForm settings={autoRouter} models={models} />
        </div>
      </TabsContent>

      <TabsContent value="boons">
        <BoonsSettingsForm settings={boons} models={models} />
      </TabsContent>

      <TabsContent value="compression">
        <CompressionSettingsForm settings={boons} compressor={compressor} />
      </TabsContent>

      <TabsContent value="knowledge">
        <Card>
          <CardContent className="flex flex-wrap items-center justify-between gap-3 py-5 text-sm">
            <span className="text-muted-foreground">
              Retrieval settings live on the Knowledge page now, next to the collections they apply to.
              {knowledge ? ` Retrieval is ${knowledge.enabled ? "on" : "off"}.` : ""}
            </span>
            <Button asChild size="sm" variant="outline"><Link href="/knowledge?tab=retrieval">Open retrieval settings</Link></Button>
          </CardContent>
        </Card>
      </TabsContent>

      <TabsContent value="energy">
        <EnergySettingsForm settings={energy} />
      </TabsContent>

      <TabsContent value="data">
        <div className="space-y-6">
          <UsageRetentionForm retention={retention} />
          <BackupRestore />
          <ResolverCacheCard />
        </div>
      </TabsContent>

      <TabsContent value="slurm">
        <SlurmSettingsForm settings={slurm} />
      </TabsContent>

      <TabsContent value="assistant">
        <CharoSettingsForm settings={charo} models={models} />
      </TabsContent>

      <TabsContent value="about">{versionCard}</TabsContent>
    </Tabs>
  );
}
