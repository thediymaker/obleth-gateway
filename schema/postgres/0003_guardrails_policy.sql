-- Idempotent: safe to re-run.

alter table tenants add column if not exists guardrails_policy jsonb;
