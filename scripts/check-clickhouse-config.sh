#!/usr/bin/env bash
# The Helm chart renders ClickHouse's config from values; Compose mounts
# deploy/docker/clickhouse/system-logs.xml. Check both say the same with the
# chart's defaults, and that the metric_log switch still renders valid XML
# with column-by-column merges.
set -euo pipefail
cd "$(dirname "$0")/.."

render() {
  helm template check deploy/k8s/obleth --show-only templates/deps.yaml \
    --set obleth.adminToken=x --set clickhouse.password=x --set postgres.password=x --set redis.password=x \
    --set controlPlane.dashboardPassword=x --set controlPlane.dashboardSessionSecret=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx "$@" |
    python3 -c '
import sys
# The literal block under "obleth.xml: |", without its indent.
lines, out, inside = sys.stdin.read().splitlines(), [], False
for line in lines:
    if line.strip() == "obleth.xml: |":
        inside = True
    elif inside and (line.startswith("    ") or not line.strip()):
        out.append(line[4:])
    elif inside:
        break
sys.stdout.write("\n".join(out) + "\n")
'
}

# The settings as text: comments and whitespace dropped, one element per line.
canonical() {
  python3 -c '
import sys, xml.etree.ElementTree as ET
def walk(e, path):
    attrs = "".join(f" {k}={v}" for k, v in sorted(e.attrib.items()))
    text = (e.text or "").strip()
    print(f"{path}/{e.tag}{attrs} {text}".rstrip())
    for c in e:
        walk(c, f"{path}/{e.tag}")
walk(ET.parse(sys.argv[1]).getroot(), "")
' "$1"
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

render >"$tmp/chart.xml"
canonical "$tmp/chart.xml" >"$tmp/chart.txt"
canonical deploy/docker/clickhouse/system-logs.xml >"$tmp/compose.txt"
if ! diff -u "$tmp/compose.txt" "$tmp/chart.txt"; then
  echo "deploy/docker/clickhouse/system-logs.xml and the chart's default ClickHouse config differ" >&2
  exit 1
fi

render --set clickhouse.metricLog=true >"$tmp/metric.xml"
canonical "$tmp/metric.xml" | grep -q '/clickhouse/metric_log/settings .*vertical_merge_algorithm_min_columns_to_activate = 1' || {
  echo "clickhouse.metricLog=true does not render metric_log with vertical merges" >&2
  exit 1
}
echo "ClickHouse config: chart and Compose agree"
