# Database Migration v2

## Purpose

Schema version 2 repairs six foreign keys that referenced `verses(id)`. `verses`
is a compatibility view and cannot be a SQLite foreign-key parent. The canonical
parent is the `verses_base(id)` primary key.

The migration is part of the ordered Rust runtime runner in
`frontend/src-tauri/src/database_migrations.rs`. It is not distributed as
executable package SQL.

## Rebuilt Tables

| Table | Primary key | Corrected verse FK action | Other FKs preserved | Baseline rows |
| --- | --- | --- | --- | ---: |
| `commentaries` | `(commentary_id, verse_id)` | `NO ACTION` | None | 934 |
| `verse_geography` | `(verse_id, place_id)` | `NO ACTION` | `place_id → geography_places(place_id)`, `NO ACTION` | 1,016 |
| `event_verses` | `(event_id, verse_id)` | `NO ACTION` | `event_id → timeline_events(event_id)`, `NO ACTION` | 956 |
| `dictionary_scripture_refs` | `id INTEGER PRIMARY KEY AUTOINCREMENT` | `ON DELETE CASCADE` | `entry_slug → dictionary_entries(slug)`, `ON DELETE CASCADE` | 31,488 |
| `relationships` | `id` | `ON DELETE SET NULL` | Both person FKs retain `ON DELETE CASCADE` | 5,450 |
| `people_verses` | `(person_id, verse_id)` | `ON DELETE CASCADE` | `person_id → people(id)`, `ON DELETE CASCADE` | 41,965 |

All corrected verse foreign keys retain `ON UPDATE NO ACTION`.

## Safe Rebuild

SQLite foreign-key enforcement is disabled before the v2 transaction because
it cannot be toggled inside an active transaction. The runner verifies that the
pragma changed, starts an immediate transaction, creates six replacement
tables, copies all rows, compares old/new counts, checks every verse reference
against `verses_base`, replaces the old tables, restores the
`dictionary_scripture_refs` autoincrement sequence, and commits. Enforcement is
then restored and verified before `PRAGMA foreign_key_check`.

Any missing table, orphaned verse, unexpected missing person, copy failure,
count mismatch, or constraint failure rolls back the complete migration.

## Legacy `NA` Integrity Sentinel

The production seed contains 14,704 `people_verses` rows with
`person_id = 'NA'`, but no matching `people` row. These are legacy unassigned
links, not missing verse references. Deleting them would violate row
preservation and dropping the person FK would weaken the schema.

Migration v2 therefore inserts exactly one technical parent when those links
exist:

| Column | Value |
| --- | --- |
| `id` | `NA` |
| `name` | `N/A` |
| `unique_attribute` | `Technical integrity sentinel` |
| `notes` | `Legacy unassigned verse-person references` |

No other missing person ID is accepted. This sentinel is integrity metadata,
not imported Biblical study content.

## Index

`idx_dictionary_scripture_refs_verse_id` supports verse-reference lookup and
SQLite cascade validation. On the July 2026 seed it occupies 520,192 bytes and
changes the lookup plan from a table scan to:

```text
SEARCH dictionary_scripture_refs USING INDEX
idx_dictionary_scripture_refs_verse_id (verse_id=?)
```

No speculative indexes were added.

## Validation

Disposable copied-seed validation requires:

```sql
PRAGMA integrity_check;     -- ok
PRAGMA foreign_key_check;   -- zero rows
```

The six affected table counts must be identical before and after migration.
Representative session HTML, session documents, `sessions_fts`, chat history,
and settings are hashed before and after in automated tests. Migration v2 does
not rebuild or update those user-owned tables.
