"use client";

import { PolarAngleAxis, RadialBar, RadialBarChart, ResponsiveContainer } from "recharts";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { formatNumber } from "@/lib/utils";

const QUEUED_COLOR = "hsl(38 75% 60%)";

export function UtilizationGaugeCard({
  util,
  inFlight,
  maxInFlight,
  queued,
  title = "Live utilization",
  description = "Global in-flight vs configured capacity",
  className,
}: {
  util: number;
  inFlight?: number;
  maxInFlight?: number;
  queued?: number;
  title?: string;
  description?: string;
  className?: string;
}) {
  const color = util > 90 ? "hsl(350 60% 58%)" : util > 70 ? "hsl(38 65% 56%)" : "hsl(158 50% 50%)";
  const data = [{ name: "util", value: Math.min(util, 100), fill: color }];

  return (
    <Card className={className ?? "h-full"}>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent className="relative h-64">
        <ResponsiveContainer width="100%" height="100%">
          <RadialBarChart innerRadius="68%" outerRadius="100%" barSize={18} data={data} startAngle={220} endAngle={-40}>
            <PolarAngleAxis type="number" domain={[0, 100]} angleAxisId={0} tick={false} />
            <RadialBar background={{ fill: "hsl(240 4% 13%)" }} dataKey="value" cornerRadius={9} angleAxisId={0} />
          </RadialBarChart>
        </ResponsiveContainer>
        <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center">
          <span className="text-4xl font-semibold tabular-nums" style={{ color }}>
            {util.toFixed(0)}%
          </span>
          {inFlight !== undefined && maxInFlight !== undefined && (
            <span className="mt-1 text-xs tabular-nums text-muted-foreground">
              {formatNumber(inFlight)} / {formatNumber(maxInFlight)} slots
            </span>
          )}
          {queued !== undefined && queued > 0 && (
            <span className="mt-0.5 text-[11px] tabular-nums" style={{ color: QUEUED_COLOR }}>
              {formatNumber(queued)} queued
            </span>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
