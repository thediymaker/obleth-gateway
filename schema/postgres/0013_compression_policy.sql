-- Idempotent: safe to re-run.

alter table tenants add column if not exists compression_policy jsonb;
