-- A claim takes a batch's pending items in line order. This index holds them in
-- that order, so a claim reads only the rows it takes instead of every due
-- pending row and a sort; next_attempt_at is checked on the rows it reads. It
-- replaces batch_items_pending_idx (batch_id, next_attempt_at, line), which
-- 0039 no longer creates.
create index if not exists batch_items_pending_line_idx
    on batch_items (batch_id, line) where status = 'pending';
drop index if exists batch_items_pending_idx;

-- The progress tick recounts only batches with an item finished since their
-- last progress; this finds them without reading each batch's items.
create index if not exists batch_items_finished_idx
    on batch_items (batch_id, finished_at) where finished_at is not null;
