"use client";
import React from "react";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import type { ClusterResources } from "@/lib/obleth";

export type ResourceValue = {
  partition: string;
  node: string;        // selected node name; "" when free-text / none chosen
  gres: string;        // auto-filled from the chosen node, but editable
  cpusPerTask: string; // string (feeds FormData later); "" = leave default
  mem: string;         // e.g. "560G"; "" = leave default
};

export function ResourceFields(props: {
  value: ResourceValue;
  onChange: (next: ResourceValue) => void;
  resources: ClusterResources | undefined;
}): React.ReactElement {
  const { value, onChange, resources } = props;

  const hasPartitions = (resources?.partitions?.length ?? 0) > 0;
  const hasNodes = (resources?.nodes?.length ?? 0) > 0;

  // Filter nodes to those belonging to the selected partition, fall back to all.
  const filteredNodes = hasNodes
    ? value.partition
      ? (resources!.nodes.filter((n) => n.partitions.includes(value.partition)))
      : resources!.nodes
    : [];
  const nodeList =
    filteredNodes.length > 0 ? filteredNodes : (resources?.nodes ?? []);

  function handlePartitionChange(partition: string) {
    // When partition changes, clear node (and consequently gres) only if the
    // currently selected node isn't valid for the new partition.
    const nodeStillValid =
      partition === "" ||
      resources?.nodes
        .find((n) => n.name === value.node)
        ?.partitions.includes(partition);
    onChange({
      ...value,
      partition,
      node: nodeStillValid ? value.node : "",
      gres: nodeStillValid ? value.gres : "",
      cpusPerTask: nodeStillValid ? value.cpusPerTask : "",
      mem: nodeStillValid ? value.mem : "",
    });
  }

  function handleNodeChange(nodeName: string) {
    const node = resources?.nodes.find((n) => n.name === nodeName);
    const gres = node ? node.gres : "";
    const cpusPerTask =
      node?.cpus != null ? String(node.cpus) : value.cpusPerTask;
    const mem =
      node?.real_memory_mb != null
        ? `${Math.round(node.real_memory_mb / 1024)}G`
        : value.mem;
    onChange({ ...value, node: nodeName, gres, cpusPerTask, mem });
  }

  const noDiscovery = !hasPartitions && !hasNodes;

  return (
    <div className="space-y-3">
      {noDiscovery && (
        <p className="text-xs text-muted-foreground/70">
          Slurm discovery unavailable - enter values manually.
        </p>
      )}

      <div className="grid gap-3 sm:grid-cols-2">
        {/* Partition */}
        <div className="space-y-1.5">
          <Label htmlFor="rf-partition">Partition</Label>
          {hasPartitions ? (
            <Select
              id="rf-partition"
              value={value.partition}
              onChange={(e) => handlePartitionChange(e.target.value)}
              className="h-9 w-full text-xs"
            >
              <option value="">Select partition...</option>
              {resources!.partitions.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                </option>
              ))}
            </Select>
          ) : (
            <Input
              id="rf-partition"
              value={value.partition}
              onChange={(e) => onChange({ ...value, partition: e.target.value })}
              placeholder="gpu-preempt"
              className="h-9 w-full text-xs"
            />
          )}
        </div>

        {/* Node */}
        <div className="space-y-1.5">
          <Label htmlFor="rf-node">Node</Label>
          {hasNodes ? (
            <Select
              id="rf-node"
              value={value.node}
              onChange={(e) => handleNodeChange(e.target.value)}
              className="h-9 w-full text-xs"
            >
              <option value="">Select node...</option>
              {nodeList.map((n) => (
                <option key={n.name} value={n.name}>
                  {n.name}
                </option>
              ))}
            </Select>
          ) : (
            <Input
              id="rf-node"
              value={value.node}
              onChange={(e) => onChange({ ...value, node: e.target.value })}
              placeholder="gpu-node-01"
              className="h-9 w-full text-xs"
            />
          )}
        </div>
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        {/* GRES */}
        <div className="space-y-1.5">
          <Label htmlFor="rf-gres">GRES</Label>
          <Input
            id="rf-gres"
            value={value.gres}
            onChange={(e) => onChange({ ...value, gres: e.target.value })}
            placeholder="gpu:1"
            className="h-9 w-full text-xs"
          />
        </div>

        {/* CPUs per task */}
        <div className="space-y-1.5">
          <Label htmlFor="rf-cpus">CPUs per task</Label>
          <Input
            id="rf-cpus"
            type="number"
            value={value.cpusPerTask}
            onChange={(e) => onChange({ ...value, cpusPerTask: e.target.value })}
            placeholder="cluster default"
            className="h-9 w-full text-xs"
          />
        </div>

        {/* Mem */}
        <div className="space-y-1.5">
          <Label htmlFor="rf-mem">Memory</Label>
          <Input
            id="rf-mem"
            value={value.mem}
            onChange={(e) => onChange({ ...value, mem: e.target.value })}
            placeholder="560G"
            className="h-9 w-full text-xs"
          />
        </div>
      </div>
    </div>
  );
}
