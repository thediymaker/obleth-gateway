-- Per-model upstream debug diagnostics: when on, a terminal 502/504 triggers a
-- read-only DNS-resolve + TCP-connect probe recorded as a trace span.
alter table models add column if not exists debug_diagnostics boolean not null default false;
