"use client";

import type { ReactNode } from "react";
import { Bell, Route, Database, Info, Server, Bot, Archive, Zap } from "lucide-react";
import {
  AlertSettingsForm,
  AutoRouterSettingsForm,
  BoonsSettingsForm,
  CharoSettingsForm,
  CompressionSettingsForm,
  SlurmSettingsForm,
  UsageRetentionForm,
} from "@/components/settings-form";
import { EnergySettingsForm } from "@/components/energy-settings-form";
import { BackupRestore } from "@/components/backup-restore";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  EnergySettingsView,
  ModelRoute,
  SlurmSettingsView,
  UsageRetentionView,
} from "@/lib/obleth";

export function SettingsTabs({
  alertSettings,
  autoRouter,
  boons,
  charo,
  compressor,
  energy,
  models,
  retention,
  slurm,
  versionCard,
}: {
  alertSettings: AlertSettingsView | null;
  autoRouter: AutoRouterSettingsView | null;
  boons: BoonSettingsView | null;
  charo: CharoSettingsView | null;
  compressor: CompressorStatusView | null;
  energy: EnergySettingsView | null;
  models: ModelRoute[];
  retention: UsageRetentionView | null;
  slurm: SlurmSettingsView | null;
  versionCard: ReactNode;
}) {
  return (
    <Tabs defaultValue="alerts">
      <TabsList>
        <TabsTrigger value="alerts">
          <Bell className="h-3.5 w-3.5" />
          Alerts
        </TabsTrigger>
        <TabsTrigger value="routing">
          <Route className="h-3.5 w-3.5" />
          Routing
        </TabsTrigger>
        <TabsTrigger value="compression">
          <Archive className="h-3.5 w-3.5" />
          Compression
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
          <AutoRouterSettingsForm settings={autoRouter} models={models} />
          <BoonsSettingsForm settings={boons} models={models} />
        </div>
      </TabsContent>

      <TabsContent value="compression">
        <CompressionSettingsForm settings={boons} compressor={compressor} />
      </TabsContent>

      <TabsContent value="energy">
        <EnergySettingsForm settings={energy} />
      </TabsContent>

      <TabsContent value="data">
        <div className="space-y-6">
          <UsageRetentionForm retention={retention} />
          <BackupRestore />
        </div>
      </TabsContent>

      <TabsContent value="slurm">
        <SlurmSettingsForm settings={slurm} />
      </TabsContent>

      <TabsContent value="assistant">
        <CharoSettingsForm settings={charo} />
      </TabsContent>

      <TabsContent value="about">{versionCard}</TabsContent>
    </Tabs>
  );
}
