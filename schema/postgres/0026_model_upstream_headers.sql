-- Idempotent: safe to re-run.
-- Operator-configured headers sent on every request to a model's upstream,
-- after the client's forwarded headers so the operator's value wins (e.g. an
-- AIBrix `routing-strategy`). A JSON object of lowercase header name -> value.
-- Values are encrypted at rest like models.api_key, since an operator may put
-- a credential in one, and the Management API returns only the names.
alter table models add column if not exists upstream_headers jsonb not null default '{}'::jsonb;
