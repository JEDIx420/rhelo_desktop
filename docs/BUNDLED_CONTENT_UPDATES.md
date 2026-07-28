# Bundled Content Updates

## Desktop Startup

The integration point is the Tauri setup path in
`frontend/src-tauri/src/lib.rs`, before `DatabaseState` is managed and before
ordinary IPC reads are available:

1. Resolve the bundled seed and writable app-data database.
2. Copy the seed only when the writable database does not exist.
3. Run ordered schema migrations.
4. Validate and record `baseline.json`.
5. Discover bundled package directories.
6. Skip recorded packages without reading or hashing their payloads.
7. Validate pending packages in ascending content version.
8. Create one consistent backup for the pending batch.
9. Apply each package transactionally.
10. Expose structured status and any optional-update warning to the frontend.

Schema migration failure aborts startup. Optional package failure is converted
to a non-sensitive warning; the old content remains usable and Rhelo retries on
the next launch.

## Schema and Content Versions

Schema and content versions are independent:

| Version | Meaning |
| --- | --- |
| Schema 1 | July 2026 production seed |
| Schema 2 | Six verse foreign keys repaired |
| Schema 3 | `content_updates` tracking |
| Content 1 | Recognized July 2026 bundled Biblical baseline |

The tracked seed remains schema v1 in this phase. Fresh installs migrate the
copy without creating a migration backup because no user data exists yet.
Existing v1 databases receive a consistent schema backup before migration.
Future generated seeds should be finalized at schema v3 using deterministic
tooling, but runtime v1 support remains required.

## Tracking Table

```sql
CREATE TABLE content_updates (
    update_id TEXT PRIMARY KEY,
    content_version INTEGER NOT NULL UNIQUE CHECK(content_version > 0),
    payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256) = 64),
    applied_at TEXT NOT NULL,
    app_version TEXT NOT NULL,
    manifest_version INTEGER NOT NULL CHECK(manifest_version > 0),
    source_version TEXT,
    row_count INTEGER NOT NULL DEFAULT 0 CHECK(row_count >= 0),
    duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0)
);
```

The table stores package identity and aggregate diagnostics only. It never
stores package bodies, SQL, sessions, chats, documents, transcripts, or
secrets. Primary-key and unique constraints provide the only required indexes.

## Baseline Recognition

The production baseline descriptor is
`frontend/src-tauri/resources/content-updates/baseline.json`. Recognition
requires schema v3 plus exact counts for key static tables and every existing
translation code, and the three known route IDs. User table contents and the
whole database checksum do not participate.

An unknown baseline produces an optional warning and blocks package application
without altering existing static or user data.

## Backup Policy

A package backup is created only when at least one validated package is
pending. Before backup, Rhelo requires free space for:

```text
database size + pending package bytes + 64 MiB safety margin
```

The rusqlite online backup API creates a SQLite-consistent file in app data,
which is reopened and checked with `PRAGMA quick_check`. Names include the
content-version range. Only the two newest content-update backups are retained.
Migration backups use separate names and are not confused with content
backups.

## FTS Policy

The current static FTS structures are either unrelated to the initial allowlist
or have fixed translation layouts that cannot safely index arbitrary new
translations. Schema v3 therefore rejects every `fts_rebuild` request.
`sessions_fts` is explicitly denied and is never touched by content updates.
A future schema must introduce a source-owned dynamic static index before this
contract can permit targeted rebuilds.

## Test-Only Package

The synthetic package under `frontend/src-tauri/tests/fixtures/` is not listed
in `tauri.conf.json` and is not shipped. Tests cover first apply, repeat skip,
corrupt checksum, malformed JSON/manifest, unknown and user tables, traversal,
size limits, invalid verses and coordinates, duplicate translation rows,
schema mismatch, out-of-order versions, disk-space failure, atomic rollback,
and retry.

No production translation, route, commentary, or other Biblical row is added
by this phase.
