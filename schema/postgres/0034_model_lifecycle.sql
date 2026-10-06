-- Idempotent: safe to re-run.
-- Where a model is in its life: active, deprecated (still served, with
-- Deprecation/Sunset headers naming a replacement and a retirement date), or
-- retired (refused with 410 Gone, or optionally served by its replacement).
-- One JSON object, {"status", "replacement", "retire_at", "changed_at",
-- "note", "redirect"}, read tolerantly like `variants`, so a field added later
-- needs no migration. Every pre-migration row reads as active.
alter table models add column if not exists lifecycle jsonb not null default '{}'::jsonb;
