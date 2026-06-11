"use client";

import type { ReactNode } from "react";
import { Bell, Route, Database, Info } from "lucide-react";
import {
  AlertSettingsForm,
  AutoRouterSettingsForm,
  BoonsSettingsForm,
  UsageRetentionForm,
} from "@/components/settings-form";
import { BackupRestore } from "@/components/backup-restore";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  ModelRoute,
  UsageRetentionView,
} from "@/lib/obleth";

export function SettingsTabs({
  alertSettings,
  autoRouter,
  boons,
  models,
  retention,
  versionCard,
}: {
  alertSettings: AlertSettingsView | null;
  autoRouter: AutoRouterSettingsView | null;
  boons: BoonSettingsView | null;
  models: ModelRoute[];
  retention: UsageRetentionView | null;
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
        <TabsTrigger value="data">
          <Database className="h-3.5 w-3.5" />
          Data
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

      <TabsContent value="data">
        <div className="space-y-6">
          <UsageRetentionForm retention={retention} />
          <BackupRestore />
        </div>
      </TabsContent>

      <TabsContent value="about">{versionCard}</TabsContent>
    </Tabs>
  );
}
