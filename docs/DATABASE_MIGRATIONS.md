# Database Migration and Recovery

`schema-version.txt` is the single repository source for the latest SQLite schema version. The Rust build script generates `CURRENT_SCHEMA_VERSION` from it. `scripts/finalize_seed_database.py`, invoked at the end of `setup.sh`, currently marks the output of legacy data migrations 000-012 as schema v1. Runtime migrations then perform the same tested v1-to-v3 upgrade used for existing installations.

The bundled seed may intentionally be older than the latest runtime schema when the app contains a tested migration path. Asset verification accepts seed versions 1 through the current schema and rejects unsupported older or future versions.

## Startup Behavior

- Fresh install: Rhelo copies the bundled seed to the platform app-data directory, migrates the new copy, and does not create a migration backup because no user data exists yet.
- Current install: Rhelo opens the existing writable database without replacing, migrating, or backing it up.
- Older install: Rhelo creates a backup beside the writable database, then runs ordered migrations in transactions.

## Backups

Backups are stored beside the writable `rhelo.db`. The first backup is named `rhelo.backup-schema-v{from}-to-v{to}.sqlite3`; collisions receive `-1`, `-2`, and higher numeric suffixes. Files are created with non-overwriting semantics through SQLite's consistent online backup API, reopened, and checked before migration, so a retry cannot replace an earlier backup.

Schema v2 and v3 are documented in `DATABASE_MIGRATION_V2.md` and `BUNDLED_CONTENT_UPDATES.md`.

Rhelo does not automatically delete migration backups. Users may archive or remove old backups after confirming the upgraded database works.

## Recovery

1. Quit Rhelo.
2. Preserve the failed writable `rhelo.db` separately for diagnosis.
3. Copy the desired migration backup into the same app-data directory.
4. Rename the copied file to `rhelo.db`.
5. Reopen a Rhelo version that supports the backup's schema, or retry the newer version after correcting the migration failure.

Do not restore over a running application, and do not test upgrades against the only copy of a user database.
