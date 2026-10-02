{{- define "obleth.fullname" -}}
{{ .Release.Name }}-obleth
{{- end -}}

{{- define "obleth.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: obleth
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Secret names are existingSecret-aware: production installs point the chart at a
pre-created Secret so real credentials never live in values files or CLI history.
When the corresponding existingSecret is empty, the chart renders and references
its own Secret instead.
*/}}
{{- define "obleth.secretName" -}}
{{- if .Values.obleth.existingSecret -}}
{{ .Values.obleth.existingSecret }}
{{- else -}}
{{ include "obleth.fullname" . }}-secret
{{- end -}}
{{- end -}}

{{- define "obleth.controlPlaneSecretName" -}}
{{- if .Values.controlPlane.existingSecret -}}
{{ .Values.controlPlane.existingSecret }}
{{- else -}}
{{ .Release.Name }}-control-plane-secret
{{- end -}}
{{- end -}}

{{/*
Pod anti-affinity for the obleth data plane, driven by .Values.affinity.antiAffinity
("soft" preferred | "hard" required | "" disabled). Emits the full `affinity:` key
so callers can `{{ include "obleth.antiAffinity" . | nindent 6 }}` under a pod spec.
*/}}
{{- define "obleth.antiAffinity" -}}
{{- $aff := .Values.affinity | default dict -}}
{{- $mode := $aff.antiAffinity | default "soft" -}}
{{- if eq $mode "soft" }}
affinity:
  podAntiAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app.kubernetes.io/name: obleth
              app.kubernetes.io/instance: {{ .Release.Name }}
{{- else if eq $mode "hard" }}
affinity:
  podAntiAffinity:
    requiredDuringSchedulingIgnoredDuringExecution:
      - topologyKey: kubernetes.io/hostname
        labelSelector:
          matchLabels:
            app.kubernetes.io/name: obleth
            app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}
{{- end -}}

{{/*
Dependency URLs are toggle-aware: when the bundled dep is enabled, use the
in-chart service name; otherwise use the operator-supplied external URL. This
keeps obleth startup correct whether deps are bundled or external.
*/}}
{{- define "obleth.databaseUrl" -}}
{{- if .Values.postgres.enabled -}}
postgres://{{ .Values.postgres.user }}:{{ required "postgres.password is required" .Values.postgres.password }}@{{ .Release.Name }}-postgres:5432/{{ .Values.postgres.db }}
{{- else -}}
{{ required "postgres.enabled=false requires postgres.external.url" .Values.postgres.external.url }}
{{- end -}}
{{- end -}}

{{/*
The bundled Redis always requires AUTH, so its URL carries the password. This
helper is only rendered into the obleth Secret, never a plain env value.
*/}}
{{- define "obleth.redisUrl" -}}
{{- if .Values.redis.enabled -}}
{{- $pw := required "redis.password is required when redis.enabled=true (set via --set or obleth.existingSecret)" .Values.redis.password -}}
{{- if regexMatch "[@:/?#%[:space:]]" $pw -}}
{{- fail "redis.password must not contain '@', ':', '/', '?', '#', '%' or whitespace: it is embedded unescaped in the Redis URL, which the gateway percent-decodes. Use a hex value, e.g. `openssl rand -hex 32`." -}}
{{- end -}}
redis://:{{ $pw }}@{{ .Release.Name }}-redis:6379
{{- else -}}
{{ required "redis.enabled=false requires redis.external.url" .Values.redis.external.url }}
{{- end -}}
{{- end -}}

{{- define "obleth.clickhouseUrl" -}}
{{- if .Values.clickhouse.enabled -}}
http://{{ .Release.Name }}-clickhouse:8123
{{- else -}}
{{ required "clickhouse.enabled=false requires clickhouse.external.url" .Values.clickhouse.external.url }}
{{- end -}}
{{- end -}}

{{- define "obleth.upstream" -}}
{{- if .Values.obleth.upstreamBaseUrl -}}
{{ .Values.obleth.upstreamBaseUrl }}
{{- else if .Values.benchmarkBackend.enabled -}}
http://{{ .Release.Name }}-benchmark-backend:8081
{{- else -}}
{{ required "set obleth.upstreamBaseUrl when benchmarkBackend.enabled=false" .Values.obleth.upstreamBaseUrl }}
{{- end -}}
{{- end -}}

{{/*
Non-empty when the kubernetes capacity source is configured: discovery on and
at least one namespace listed. Gates the gateway ServiceAccount, its per-
namespace EndpointSlice-read Roles and RoleBindings, and the pod's use of that
account.
*/}}
{{- define "obleth.capacityDiscoveryRbac" -}}
{{- $cd := .Values.obleth.capacityDiscovery | default dict -}}
{{- if and $cd.enabled $cd.namespaces -}}true{{- end -}}
{{- end -}}

{{/*
config.d override for the bundled ClickHouse. The stock image logs at trace
and keeps its own system log tables (query_log, text_log, metric_log, ...)
forever, which on a busy gateway outgrows the usage ledger many times over.
Each listed table is partitioned by day and expires after
clickhouse.systemLogRetentionDays; with ttl_only_drop_parts an expired day is
dropped whole rather than rewritten. text_log keeps warnings and errors only.
Tables the image already bounds (asynchronous_insert_log, blob_storage_log)
and opentelemetry_span_log, whose custom engine takes no TTL, are left alone.
*/}}
{{- define "obleth.clickhouseConfig" -}}
{{- $days := int .Values.clickhouse.systemLogRetentionDays -}}
{{- if lt $days 1 -}}
{{- fail "clickhouse.systemLogRetentionDays must be at least 1" -}}
{{- end -}}
<clickhouse>
    <logger>
        <level>{{ .Values.clickhouse.logLevel | default "information" }}</level>
    </logger>
{{- range list "query_log" "query_thread_log" "query_views_log" "part_log" "trace_log" "text_log" "metric_log" "error_log" "asynchronous_metric_log" "processors_profile_log" "backup_log" }}
    <{{ . }}>
        <partition_by>event_date</partition_by>
        <ttl>event_date + INTERVAL {{ $days }} DAY DELETE</ttl>
        <settings>ttl_only_drop_parts = 1</settings>
        {{- if eq . "text_log" }}
        <level>warning</level>
        {{- end }}
    </{{ . }}>
{{- end }}
</clickhouse>
{{- end -}}
