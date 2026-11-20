# Postgres schema

The gateway embeds every `NNNN_name.sql` file here (listed in `MIGRATIONS` in
`obleth/crates/obleth-store/src/lib.rs`) and applies them itself at start; see
`Store::migrate`.

## How files are applied

- Under an advisory lock, the gateway runs each file that `schema_migrations`
  does not list, in order. A file and its `schema_migrations` row commit in one
  transaction, so a file that fails leaves nothing behind and runs again at the
  next start.
- A database with no `schema_migrations` rows (a new one, or one last run by a
  gateway from before versioning, which re-ran every file at each start) runs
  every file once and then records them all. That is why files up to `0047`
  are re-runnable and must stay so.
- `OBLETH_SCHEMA_VERIFY_ONLY=true` applies nothing and stops startup while any
  file is unrecorded.

## Adding a change

- Add a new file with the next number, and add it to `MIGRATIONS`. Never edit a
  file a released gateway has applied: databases that recorded it will not run
  it again. To undo something, write a new file.
- Keep files re-runnable anyway (`if not exists`, `drop ... if exists`). During
  a rolling upgrade, pods from before versioning still re-run every file at
  start, and the baseline run above re-runs them all once.
- A file runs in a transaction: no `create index concurrently`, and no
  `begin`/`commit` of its own.
- Old and new pods share the database during a rollout, so a change must work
  for the previous release too: add columns with defaults first, and drop what
  the previous release reads only in a later release.

## Conventions (from `0048` on)

Earlier files use several forms for the same thing; these rules apply to new
columns. Existing columns are not migrated.

- **Lists.** A list of scalar values (names, ids, CIDRs) is a native array
  (`text[]`, `uuid[]`), filtered with `= any(...)` and decoded as `Vec<T>`.
  A structured value is `jsonb`. A list of references that must disappear when
  the referenced row is deleted is a join table with `on delete cascade`.
- **Integers.** `bigint` for counts, limits, sizes and anything the store
  decodes into `i64`. `integer` only where the value is an `i32` in Rust and
  bounded well below 2^31 (an ordinal within one document, for example).
- **Vocabularies.** Allowed values of a text column (a status, a mode, a kind)
  are checked in `obleth-config`, where the admin API reports a bad value as a
  `400` naming the field. Postgres does not check them: a `CHECK` had to be
  rewritten for every new value, and a stale one (`0011`) once failed saves in
  production. `0048` dropped the existing vocabulary checks.
- **Ranges.** Numeric bounds (`weight >= 1`, `port between 1 and 65535`) stay
  as `CHECK` constraints, named `<table>_<column>_check`.
- **Timestamps.** `timestamptz`, with `created_at` and, on rows that change,
  `updated_at`.
- **Indexes.** Add one for a query the store runs, and say which query in a
  comment. Not on tables of tens of rows (a sequential scan is faster), and not
  one whose columns lead another index.
- **Header.** Each file starts with a comment saying what it changes and why.
