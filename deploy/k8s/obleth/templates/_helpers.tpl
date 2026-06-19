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
{{- $mode := .Values.affinity.antiAffinity -}}
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

{{- define "obleth.redisUrl" -}}
{{- if .Values.redis.enabled -}}
redis://{{ .Release.Name }}-redis:6379
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
