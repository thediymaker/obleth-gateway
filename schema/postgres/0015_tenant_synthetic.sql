-- Idempotent: safe to re-run.
-- Synthetic tenants generate benchmark/test traffic; the proxy tags their
-- ledger rows as request_type='benchmark' so default stats exclude them.
alter table tenants add column if not exists synthetic boolean not null default false;
