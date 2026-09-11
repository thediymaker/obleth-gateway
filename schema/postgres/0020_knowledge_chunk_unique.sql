-- Idempotent: safe to re-run.
--
-- Nothing enforced that a document's chunk generation couldn't collide.
-- `commit_document_generation_once` deletes a generation's rows then inserts
-- fresh ones inside one transaction, but under READ COMMITTED a second
-- worker's `delete ... where generation = $2` cannot see a first worker's
-- still-uncommitted inserts for that same generation -- so two workers
-- racing to commit the *same* document at the *same* generation (both
-- deriving `active_generation + 1` from the same snapshot) can both insert,
-- leaving duplicate `(document_id, generation, ordinal)` rows: the same
-- passage retrieved and injected twice, and `chunk_count` silently
-- under-reporting (it records only the last committer's count).
--
-- This table is new in this same unreleased change set, so no deployed
-- database can already hold a duplicate that would make index creation
-- fail here.
create unique index if not exists knowledge_chunks_document_generation_ordinal_uq
    on knowledge_chunks (document_id, generation, ordinal);
