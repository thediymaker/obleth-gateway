-- Idempotent: safe to re-run.
-- A key that fronts many end users (a chatbot, a shared front end) can name
-- each one per request; with this on, each named end user queues on their
-- own inside the key's tenant instead of sharing the key's one place.
-- Trusted, so off by default: a caller could otherwise split itself into
-- many "users" to jump the queue.
alter table api_keys add column if not exists end_user_fairshare boolean not null default false;
