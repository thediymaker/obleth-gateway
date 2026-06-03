{{- define "obleth.fullname" -}}
{{ .Release.Name }}-obleth
{{- end -}}

{{- define "obleth.labels" -}}
app.kubernetes.io/name: obleth
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
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
