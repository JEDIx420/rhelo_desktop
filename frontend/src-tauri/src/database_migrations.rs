use rusqlite::{Connection, DatabaseName, Transaction, TransactionBehavior};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::CURRENT_SCHEMA_VERSION;

pub(crate) const DATABASE_BACKUP_PREFIX: &str = "rhelo.backup-schema";

type MigrationFn = fn(&Transaction<'_>) -> Result<(), String>;

struct Migration {
    version: i32,
    requires_foreign_keys_off: bool,
    apply: MigrationFn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SchemaMigrationReport {
    pub(crate) from_version: i32,
    pub(crate) to_version: i32,
    pub(crate) applied_versions: Vec<i32>,
    pub(crate) backup_path: Option<PathBuf>,
    pub(crate) duration_ms: u128,
}

pub(crate) fn open_database(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path)
        .map_err(|error| format!("Failed to open the Rhelo database: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| format!("Failed to configure SQLite busy timeout: {error}"))?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| format!("Failed to configure SQLite: {error}"))?;
    Ok(connection)
}

pub(crate) fn read_user_version(connection: &Connection) -> Result<i32, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("Failed to read SQLite user_version: {error}"))
}

pub(crate) fn backup_path_for_suffix(
    database_path: &Path,
    from_version: i32,
    to_version: i32,
    suffix: usize,
) -> PathBuf {
    let parent = database_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if suffix == 0 {
        parent.join(format!(
            "{DATABASE_BACKUP_PREFIX}-v{from_version}-to-v{to_version}.sqlite3"
        ))
    } else {
        parent.join(format!(
            "{DATABASE_BACKUP_PREFIX}-v{from_version}-to-v{to_version}-{suffix}.sqlite3"
        ))
    }
}

fn reserve_backup_path(
    database_path: &Path,
    from_version: i32,
    to_version: i32,
) -> Result<PathBuf, String> {
    for suffix in 0.. {
        let backup_path = backup_path_for_suffix(database_path, from_version, to_version, suffix);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(_) => return Ok(backup_path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to reserve database backup path {:?}: {error}",
                    backup_path
                ))
            }
        }
    }
    unreachable!("the collision-safe backup suffix space is unbounded")
}

pub(crate) fn create_consistent_database_backup(
    database_path: &Path,
    backup_path: &Path,
) -> Result<(), String> {
    let source = open_database(database_path)?;
    source
        .backup(DatabaseName::Main, backup_path, None)
        .map_err(|error| {
            let _ = fs::remove_file(backup_path);
            format!(
                "Failed to create a consistent database backup at {:?}: {error}",
                backup_path
            )
        })?;

    let backup = open_database(backup_path).map_err(|error| {
        let _ = fs::remove_file(backup_path);
        format!("The database backup could not be reopened: {error}")
    })?;
    let quick_check: String = backup
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| {
            let _ = fs::remove_file(backup_path);
            format!("The database backup could not be verified: {error}")
        })?;
    if quick_check != "ok" {
        drop(backup);
        let _ = fs::remove_file(backup_path);
        return Err("The database backup failed its SQLite quick check.".to_string());
    }
    Ok(())
}

pub(crate) fn create_schema_backup(
    database_path: &Path,
    from_version: i32,
    to_version: i32,
) -> Result<PathBuf, String> {
    let backup_path = reserve_backup_path(database_path, from_version, to_version)?;
    create_consistent_database_backup(database_path, &backup_path)?;
    Ok(backup_path)
}

fn migration_001_baseline(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS session_documents (
                document_id TEXT PRIMARY KEY,
                session_id TEXT,
                file_path TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
                session_id UNINDEXED,
                title,
                content
            );

            CREATE TRIGGER IF NOT EXISTS trg_sessions_ai AFTER INSERT ON sessions BEGIN
                INSERT INTO sessions_fts (session_id, title, content)
                VALUES (new.session_id, new.title, new.content);
            END;

            CREATE TRIGGER IF NOT EXISTS trg_sessions_ad AFTER DELETE ON sessions BEGIN
                DELETE FROM sessions_fts WHERE session_id = old.session_id;
            END;

            CREATE TRIGGER IF NOT EXISTS trg_sessions_au AFTER UPDATE ON sessions BEGIN
                UPDATE sessions_fts
                SET title = new.title, content = new.content
                WHERE session_id = old.session_id;
            END;
            ",
        )
        .map_err(|error| {
            format!("Migration 1 failed while ensuring study-session tables: {error}")
        })?;

    transaction
        .execute(
            "
            INSERT INTO sessions_fts (session_id, title, content)
            SELECT s.session_id, s.title, s.content
            FROM sessions s
            WHERE NOT EXISTS (
                SELECT 1
                FROM sessions_fts f
                WHERE f.session_id = s.session_id
            )
            ",
            [],
        )
        .map_err(|error| format!("Migration 1 failed while syncing sessions_fts: {error}"))?;
    Ok(())
}

fn verify_rebuild_counts(
    transaction: &Transaction<'_>,
    old_table: &str,
    new_table: &str,
) -> Result<(), String> {
    let old_count: i64 = transaction
        .query_row(&format!("SELECT COUNT(*) FROM {old_table}"), [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("Migration 2 could not count {old_table}: {error}"))?;
    let new_count: i64 = transaction
        .query_row(&format!("SELECT COUNT(*) FROM {new_table}"), [], |row| {
            row.get(0)
        })
        .map_err(|error| format!("Migration 2 could not count {new_table}: {error}"))?;
    if old_count != new_count {
        return Err(format!(
            "Migration 2 row-count mismatch for {old_table}: expected {old_count}, copied {new_count}"
        ));
    }
    Ok(())
}

fn verify_no_verse_orphans(transaction: &Transaction<'_>, table: &str) -> Result<(), String> {
    let orphan_count: i64 = transaction
        .query_row(
            &format!(
                "SELECT COUNT(*)
                 FROM {table} child
                 WHERE child.verse_id IS NOT NULL
                   AND NOT EXISTS (
                     SELECT 1 FROM verses_base parent WHERE parent.id = child.verse_id
                   )"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Migration 2 could not validate {table}: {error}"))?;
    if orphan_count != 0 {
        return Err(format!(
            "Migration 2 refused to rebuild {table}: {orphan_count} verse references are orphaned"
        ));
    }
    Ok(())
}

fn migration_002_repair_biblical_foreign_keys(transaction: &Transaction<'_>) -> Result<(), String> {
    let expected_tables = [
        "commentaries",
        "verse_geography",
        "event_verses",
        "dictionary_scripture_refs",
        "relationships",
        "people_verses",
    ];
    let present_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table'
               AND name IN (
                 'commentaries',
                 'verse_geography',
                 'event_verses',
                 'dictionary_scripture_refs',
                 'relationships',
                 'people_verses'
               )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Migration 2 could not inspect the affected tables: {error}"))?;
    if present_count != expected_tables.len() as i64 {
        return Err(format!(
            "Migration 2 requires all six affected Biblical tables; found {present_count}"
        ));
    }

    for table in expected_tables {
        verify_no_verse_orphans(transaction, table)?;
    }

    let unexpected_missing_people: i64 = transaction
        .query_row(
            "SELECT COUNT(DISTINCT child.person_id)
             FROM people_verses child
             LEFT JOIN people parent ON parent.id = child.person_id
             WHERE parent.id IS NULL AND child.person_id <> 'NA'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            format!("Migration 2 could not validate people_verses parent references: {error}")
        })?;
    if unexpected_missing_people != 0 {
        return Err(format!(
            "Migration 2 found {unexpected_missing_people} unexpected missing people records"
        ));
    }
    let unassigned_reference_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM people_verses WHERE person_id = 'NA'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            format!("Migration 2 could not inspect legacy unassigned people references: {error}")
        })?;
    if unassigned_reference_count > 0 {
        transaction
            .execute(
                "INSERT INTO people (id, name, sex, tribe, unique_attribute, notes)
                 SELECT
                    'NA',
                    'N/A',
                    NULL,
                    NULL,
                    'Technical integrity sentinel',
                    'Legacy unassigned verse-person references'
                 WHERE NOT EXISTS (SELECT 1 FROM people WHERE id = 'NA')",
                [],
            )
            .map_err(|error| {
                format!("Migration 2 could not establish the legacy NA parent sentinel: {error}")
            })?;
    }

    transaction
        .execute_batch(
            "
            CREATE TABLE commentaries__v2 (
                commentary_id TEXT,
                verse_id TEXT,
                text TEXT,
                PRIMARY KEY(commentary_id, verse_id),
                FOREIGN KEY(verse_id) REFERENCES verses_base(id)
            );
            INSERT INTO commentaries__v2 (commentary_id, verse_id, text)
            SELECT commentary_id, verse_id, text FROM commentaries;

            CREATE TABLE verse_geography__v2 (
                verse_id TEXT,
                place_id TEXT,
                FOREIGN KEY(verse_id) REFERENCES verses_base(id),
                FOREIGN KEY(place_id) REFERENCES geography_places(place_id),
                PRIMARY KEY(verse_id, place_id)
            );
            INSERT INTO verse_geography__v2 (verse_id, place_id)
            SELECT verse_id, place_id FROM verse_geography;

            CREATE TABLE event_verses__v2 (
                event_id TEXT,
                verse_id TEXT,
                PRIMARY KEY(event_id, verse_id),
                FOREIGN KEY(event_id) REFERENCES timeline_events(event_id),
                FOREIGN KEY(verse_id) REFERENCES verses_base(id)
            );
            INSERT INTO event_verses__v2 (event_id, verse_id)
            SELECT event_id, verse_id FROM event_verses;

            CREATE TABLE dictionary_scripture_refs__v2 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_slug TEXT NOT NULL
                    REFERENCES dictionary_entries(slug) ON DELETE CASCADE,
                verse_id TEXT NOT NULL
                    REFERENCES verses_base(id) ON DELETE CASCADE
            );
            INSERT INTO dictionary_scripture_refs__v2 (id, entry_slug, verse_id)
            SELECT id, entry_slug, verse_id FROM dictionary_scripture_refs;

            CREATE TABLE relationships__v2 (
                id TEXT PRIMARY KEY,
                person_id_1 TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                relationship_type TEXT NOT NULL,
                person_id_2 TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                verse_id TEXT REFERENCES verses_base(id) ON DELETE SET NULL,
                notes TEXT
            );
            INSERT INTO relationships__v2 (
                id, person_id_1, relationship_type, person_id_2, verse_id, notes
            )
            SELECT id, person_id_1, relationship_type, person_id_2, verse_id, notes
            FROM relationships;

            CREATE TABLE people_verses__v2 (
                person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                verse_id TEXT NOT NULL REFERENCES verses_base(id) ON DELETE CASCADE,
                PRIMARY KEY (person_id, verse_id)
            );
            INSERT INTO people_verses__v2 (person_id, verse_id)
            SELECT person_id, verse_id FROM people_verses;
            ",
        )
        .map_err(|error| format!("Migration 2 failed while copying corrected tables: {error}"))?;

    for (old_table, new_table) in [
        ("commentaries", "commentaries__v2"),
        ("verse_geography", "verse_geography__v2"),
        ("event_verses", "event_verses__v2"),
        ("dictionary_scripture_refs", "dictionary_scripture_refs__v2"),
        ("relationships", "relationships__v2"),
        ("people_verses", "people_verses__v2"),
    ] {
        verify_rebuild_counts(transaction, old_table, new_table)?;
    }

    transaction
        .execute_batch(
            "
            DROP TABLE commentaries;
            ALTER TABLE commentaries__v2 RENAME TO commentaries;

            DROP TABLE verse_geography;
            ALTER TABLE verse_geography__v2 RENAME TO verse_geography;

            DROP TABLE event_verses;
            ALTER TABLE event_verses__v2 RENAME TO event_verses;

            DROP TABLE dictionary_scripture_refs;
            ALTER TABLE dictionary_scripture_refs__v2
                RENAME TO dictionary_scripture_refs;
            DELETE FROM sqlite_sequence WHERE name = 'dictionary_scripture_refs';
            INSERT INTO sqlite_sequence (name, seq)
            SELECT 'dictionary_scripture_refs', COALESCE(MAX(id), 0)
            FROM dictionary_scripture_refs;
            CREATE INDEX idx_dictionary_scripture_refs_verse_id
                ON dictionary_scripture_refs(verse_id);

            DROP TABLE relationships;
            ALTER TABLE relationships__v2 RENAME TO relationships;

            DROP TABLE people_verses;
            ALTER TABLE people_verses__v2 RENAME TO people_verses;
            ",
        )
        .map_err(|error| format!("Migration 2 failed while replacing affected tables: {error}"))?;
    Ok(())
}

fn migration_003_content_update_tracking(transaction: &Transaction<'_>) -> Result<(), String> {
    transaction
        .execute_batch(
            "
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
            ",
        )
        .map_err(|error| format!("Migration 3 failed while adding content tracking: {error}"))
}

fn ordered_migrations() -> &'static [Migration] {
    &[
        Migration {
            version: 1,
            requires_foreign_keys_off: false,
            apply: migration_001_baseline,
        },
        Migration {
            version: 2,
            requires_foreign_keys_off: true,
            apply: migration_002_repair_biblical_foreign_keys,
        },
        Migration {
            version: 3,
            requires_foreign_keys_off: false,
            apply: migration_003_content_update_tracking,
        },
    ]
}

fn set_foreign_keys(connection: &Connection, enabled: bool) -> Result<(), String> {
    connection
        .execute_batch(if enabled {
            "PRAGMA foreign_keys = ON;"
        } else {
            "PRAGMA foreign_keys = OFF;"
        })
        .map_err(|error| format!("Failed to configure foreign-key enforcement: {error}"))?;
    let actual: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(|error| format!("Failed to verify foreign-key enforcement: {error}"))?;
    if actual != i64::from(enabled) {
        return Err("SQLite did not apply the requested foreign-key mode.".to_string());
    }
    Ok(())
}

fn verify_foreign_keys(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| format!("Foreign-key verification could not start: {error}"))?;
    let mut rows = statement
        .query([])
        .map_err(|error| format!("Foreign-key verification failed: {error}"))?;
    if rows
        .next()
        .map_err(|error| format!("Foreign-key verification failed: {error}"))?
        .is_some()
    {
        return Err("SQLite foreign_key_check reported a violation.".to_string());
    }
    Ok(())
}

fn verify_integrity(connection: &Connection) -> Result<(), String> {
    let result: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("SQLite integrity_check failed to run: {error}"))?;
    if result != "ok" {
        return Err(format!("SQLite integrity_check failed: {result}"));
    }
    Ok(())
}

pub(crate) fn apply_migrations(
    connection: &mut Connection,
    from_version: i32,
) -> Result<Vec<i32>, String> {
    if from_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "The writable database schema version ({from_version}) is newer than this app supports ({CURRENT_SCHEMA_VERSION})."
        ));
    }
    let mut expected_version = from_version + 1;
    let mut applied = Vec::new();
    for migration in ordered_migrations() {
        if migration.version <= from_version {
            continue;
        }
        if migration.version != expected_version {
            return Err(format!(
                "Database migration sequence has a gap before version {}.",
                migration.version
            ));
        }

        if migration.requires_foreign_keys_off {
            set_foreign_keys(connection, false)?;
        } else {
            set_foreign_keys(connection, true)?;
        }

        let migration_result = (|| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| {
                    format!(
                        "Failed to start migration transaction {}: {error}",
                        migration.version
                    )
                })?;
            (migration.apply)(&transaction)?;
            transaction
                .execute_batch(&format!("PRAGMA user_version = {};", migration.version))
                .map_err(|error| {
                    format!(
                        "Migration {} failed while updating user_version: {error}",
                        migration.version
                    )
                })?;
            transaction.commit().map_err(|error| {
                format!("Failed to commit migration {}: {error}", migration.version)
            })
        })();

        let foreign_key_restore = set_foreign_keys(connection, true);
        migration_result?;
        foreign_key_restore?;
        if migration.version == 2 {
            verify_foreign_keys(connection)?;
        }
        applied.push(migration.version);
        expected_version += 1;
    }
    Ok(applied)
}

pub(crate) fn ensure_database_schema(
    database_path: &Path,
    create_backup: bool,
) -> Result<SchemaMigrationReport, String> {
    let started = Instant::now();
    let mut connection = open_database(database_path)?;
    let user_version = read_user_version(&connection)?;
    if user_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "The writable database schema version ({user_version}) is newer than this app supports ({CURRENT_SCHEMA_VERSION})."
        ));
    }
    if user_version == CURRENT_SCHEMA_VERSION {
        return Ok(SchemaMigrationReport {
            from_version: user_version,
            to_version: user_version,
            applied_versions: Vec::new(),
            backup_path: None,
            duration_ms: started.elapsed().as_millis(),
        });
    }

    let backup_path = if create_backup {
        Some(create_schema_backup(
            database_path,
            user_version,
            CURRENT_SCHEMA_VERSION,
        )?)
    } else {
        None
    };
    let applied_versions = apply_migrations(&mut connection, user_version).map_err(|error| {
        match &backup_path {
            Some(path) => format!(
                "Database migration failed at schema version {user_version}. Backup preserved at {:?}. {error}",
                path
            ),
            None => format!("Fresh-install database migration failed: {error}"),
        }
    })?;
    verify_foreign_keys(&connection)?;
    verify_integrity(&connection)?;

    Ok(SchemaMigrationReport {
        from_version: user_version,
        to_version: CURRENT_SCHEMA_VERSION,
        applied_versions,
        backup_path,
        duration_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn temp_database(label: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rhelo-migration-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        directory.join("rhelo.sqlite3")
    }

    fn create_representative_v1(path: &Path, orphan_commentary: bool) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = OFF;
                CREATE TABLE verses_base (id TEXT PRIMARY KEY);
                CREATE VIEW verses AS SELECT id FROM verses_base;
                CREATE TABLE geography_places (place_id TEXT PRIMARY KEY);
                CREATE TABLE timeline_events (event_id TEXT PRIMARY KEY);
                CREATE TABLE dictionary_entries (slug TEXT PRIMARY KEY);
                CREATE TABLE people (id TEXT PRIMARY KEY);

                CREATE TABLE commentaries (
                    commentary_id TEXT,
                    verse_id TEXT,
                    text TEXT,
                    PRIMARY KEY(commentary_id, verse_id),
                    FOREIGN KEY(verse_id) REFERENCES verses(id)
                );
                CREATE TABLE verse_geography (
                    verse_id TEXT,
                    place_id TEXT,
                    FOREIGN KEY(verse_id) REFERENCES verses(id),
                    FOREIGN KEY(place_id) REFERENCES geography_places(place_id),
                    PRIMARY KEY(verse_id, place_id)
                );
                CREATE TABLE event_verses (
                    event_id TEXT,
                    verse_id TEXT,
                    PRIMARY KEY(event_id, verse_id),
                    FOREIGN KEY(event_id) REFERENCES timeline_events(event_id),
                    FOREIGN KEY(verse_id) REFERENCES verses(id)
                );
                CREATE TABLE dictionary_scripture_refs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    entry_slug TEXT NOT NULL
                        REFERENCES dictionary_entries(slug) ON DELETE CASCADE,
                    verse_id TEXT NOT NULL REFERENCES verses(id) ON DELETE CASCADE
                );
                CREATE TABLE relationships (
                    id TEXT PRIMARY KEY,
                    person_id_1 TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                    relationship_type TEXT NOT NULL,
                    person_id_2 TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                    verse_id TEXT REFERENCES verses(id) ON DELETE SET NULL,
                    notes TEXT
                );
                CREATE TABLE people_verses (
                    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
                    verse_id TEXT NOT NULL REFERENCES verses(id) ON DELETE CASCADE,
                    PRIMARY KEY (person_id, verse_id)
                );

                CREATE TABLE sessions (
                    session_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    content TEXT,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE session_documents (
                    document_id TEXT PRIMARY KEY,
                    session_id TEXT,
                    file_path TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
                );
                CREATE VIRTUAL TABLE sessions_fts USING fts5(
                    session_id UNINDEXED, title, content
                );
                CREATE TRIGGER trg_sessions_ai AFTER INSERT ON sessions BEGIN
                    INSERT INTO sessions_fts (session_id, title, content)
                    VALUES (new.session_id, new.title, new.content);
                END;
                CREATE TRIGGER trg_sessions_ad AFTER DELETE ON sessions BEGIN
                    DELETE FROM sessions_fts WHERE session_id = old.session_id;
                END;
                CREATE TRIGGER trg_sessions_au AFTER UPDATE ON sessions BEGIN
                    UPDATE sessions_fts SET title = new.title, content = new.content
                    WHERE session_id = old.session_id;
                END;
                CREATE TABLE chat_history (
                    message_id TEXT PRIMARY KEY,
                    session_id TEXT,
                    role TEXT,
                    content TEXT,
                    timestamp TEXT
                );
                CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

                INSERT INTO verses_base VALUES ('GEN.1.1'), ('GEN.1.2');
                INSERT INTO geography_places VALUES ('place-1');
                INSERT INTO timeline_events VALUES ('event-1');
                INSERT INTO dictionary_entries VALUES ('entry-1');
                INSERT INTO people VALUES ('person-1'), ('person-2');
                INSERT INTO verse_geography VALUES ('GEN.1.1', 'place-1');
                INSERT INTO event_verses VALUES ('event-1', 'GEN.1.1');
                INSERT INTO dictionary_scripture_refs VALUES (7, 'entry-1', 'GEN.1.1');
                INSERT INTO relationships VALUES (
                    'relationship-1', 'person-1', 'sibling',
                    'person-2', 'GEN.1.1', 'Preserve notes'
                );
                INSERT INTO people_verses VALUES ('person-1', 'GEN.1.1');

                INSERT INTO sessions (
                    session_id, title, content, updated_at
                ) VALUES (
                    'session-1', 'Formatted Session',
                    '<h2>Study</h2><blockquote><strong>Keep exactly</strong></blockquote>',
                    '2026-07-28T00:00:00Z'
                );
                INSERT INTO session_documents VALUES (
                    'document-1', 'session-1', '/safe/reference.pdf',
                    '2026-07-28T00:00:00Z'
                );
                INSERT INTO chat_history VALUES (
                    'message-1', 'session-1', 'user',
                    'Private preserved message', '2026-07-28T00:00:00Z'
                );
                INSERT INTO settings VALUES
                    ('theme', 'light'),
                    ('translation_order', '[\"rhelo-bsb\",\"rhelo-web\",\"rhelo-kjv\"]');
                PRAGMA user_version = 1;
                ",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO commentaries VALUES ('commentary-1', ?1, 'Preserve commentary')",
                [if orphan_commentary {
                    "BAD.1.1"
                } else {
                    "GEN.1.1"
                }],
            )
            .unwrap();
    }

    fn user_data_hash(connection: &Connection) -> String {
        let mut values = Vec::new();
        for sql in [
            "SELECT session_id || '|' || title || '|' || content || '|' || updated_at
             FROM sessions ORDER BY session_id",
            "SELECT document_id || '|' || session_id || '|' || file_path || '|' || created_at
             FROM session_documents ORDER BY document_id",
            "SELECT session_id || '|' || title || '|' || content
             FROM sessions_fts ORDER BY session_id",
            "SELECT message_id || '|' || session_id || '|' || role || '|' || content || '|' || timestamp
             FROM chat_history ORDER BY message_id",
            "SELECT key || '|' || value FROM settings ORDER BY key",
        ] {
            let rows = connection
                .prepare(sql)
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| row.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap();
            values.extend(rows);
        }
        format!("{:x}", Sha256::digest(values.join("\n").as_bytes()))
    }

    fn affected_counts(connection: &Connection) -> BTreeMap<String, i64> {
        [
            "commentaries",
            "verse_geography",
            "event_verses",
            "dictionary_scripture_refs",
            "relationships",
            "people_verses",
        ]
        .into_iter()
        .map(|table| {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table.to_string(), count)
        })
        .collect()
    }

    fn verse_foreign_key(connection: &Connection, table: &str) -> (String, String, String) {
        connection
            .prepare(&format!("PRAGMA foreign_key_list('{table}')"))
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    })?
                    .find_map(|row| match row {
                        Ok((parent, update, delete)) if parent == "verses_base" => {
                            Some(Ok((parent, update, delete)))
                        }
                        Ok(_) => None,
                        Err(error) => Some(Err(error)),
                    })
                    .transpose()
                    .map(|value| value.unwrap())
            })
            .unwrap()
    }

    fn dictionary_lookup_ms(connection: &Connection, iterations: usize) -> u128 {
        let started = Instant::now();
        let mut statement = connection
            .prepare(
                "SELECT COUNT(*) FROM dictionary_scripture_refs
                 WHERE verse_id = 'GEN.1.1'",
            )
            .unwrap();
        for _ in 0..iterations {
            let _: i64 = statement.query_row([], |row| row.get(0)).unwrap();
        }
        started.elapsed().as_millis()
    }

    #[test]
    fn version_one_to_three_repairs_all_foreign_keys_and_preserves_user_data() {
        let path = temp_database("v1-to-v3");
        create_representative_v1(&path, false);
        let before = open_database(&path).unwrap();
        let before_counts = affected_counts(&before);
        let before_hash = user_data_hash(&before);
        drop(before);

        let report = ensure_database_schema(&path, false).unwrap();
        assert_eq!(report.from_version, 1);
        assert_eq!(report.to_version, 3);
        assert_eq!(report.applied_versions, vec![2, 3]);
        assert!(report.backup_path.is_none());

        let after = open_database(&path).unwrap();
        assert_eq!(read_user_version(&after).unwrap(), 3);
        assert_eq!(affected_counts(&after), before_counts);
        assert_eq!(user_data_hash(&after), before_hash);
        for (table, expected_delete) in [
            ("commentaries", "NO ACTION"),
            ("verse_geography", "NO ACTION"),
            ("event_verses", "NO ACTION"),
            ("dictionary_scripture_refs", "CASCADE"),
            ("relationships", "SET NULL"),
            ("people_verses", "CASCADE"),
        ] {
            assert_eq!(
                verse_foreign_key(&after, table),
                (
                    "verses_base".to_string(),
                    "NO ACTION".to_string(),
                    expected_delete.to_string()
                )
            );
        }
        assert_eq!(
            after
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('dictionary_scripture_refs')
                     WHERE name = 'idx_dictionary_scripture_refs_verse_id'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            after
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let mut foreign_check = after.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(foreign_check.query([]).unwrap().next().unwrap().is_none());
        assert_eq!(
            after
                .query_row("SELECT COUNT(*) FROM content_updates", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        after
            .execute(
                "INSERT INTO dictionary_scripture_refs (entry_slug, verse_id)
                 VALUES ('entry-1', 'GEN.1.2')",
                [],
            )
            .unwrap();
        assert!(
            after
                .query_row("SELECT MAX(id) FROM dictionary_scripture_refs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
                > 7
        );
    }

    #[test]
    fn migration_is_idempotent_and_future_versions_are_rejected() {
        let path = temp_database("idempotent");
        create_representative_v1(&path, false);
        ensure_database_schema(&path, false).unwrap();
        let second = ensure_database_schema(&path, true).unwrap();
        assert!(second.applied_versions.is_empty());
        assert!(second.backup_path.is_none());

        let future = open_database(&path).unwrap();
        future.execute_batch("PRAGMA user_version = 999;").unwrap();
        drop(future);
        assert!(ensure_database_schema(&path, true)
            .unwrap_err()
            .contains("newer than this app supports"));
    }

    #[test]
    fn orphaned_v1_data_aborts_atomically() {
        let path = temp_database("orphan");
        create_representative_v1(&path, true);
        let error = ensure_database_schema(&path, false).unwrap_err();
        assert!(error.contains("orphaned"));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(read_user_version(&connection).unwrap(), 1);
        let schema: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'commentaries'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(schema.contains("REFERENCES verses(id)"));
        assert_eq!(affected_counts(&connection)["commentaries"], 1);
    }

    #[test]
    fn bundled_database_copy_migrates_with_integrity_and_foreign_keys_intact() {
        let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rhelo.db");
        if !bundled.exists() {
            return;
        }
        let path = temp_database("bundled-copy");
        let copy_started = Instant::now();
        fs::copy(&bundled, &path).unwrap();
        let copy_ms = copy_started.elapsed().as_millis();
        let before = Connection::open(&path).unwrap();
        let before_counts = affected_counts(&before);
        let before_bytes = fs::metadata(&path).unwrap().len();
        let before_lookup_ms = dictionary_lookup_ms(&before, 500);
        drop(before);

        let backup_path = path.with_file_name("rhelo-content-backup.sqlite3");
        let backup_started = Instant::now();
        create_consistent_database_backup(&path, &backup_path).unwrap();
        let backup_ms = backup_started.elapsed().as_millis();
        let migration_started = Instant::now();
        let report = ensure_database_schema(&path, false).unwrap();
        let migration_ms = migration_started.elapsed().as_millis();
        let after = open_database(&path).unwrap();
        assert_eq!(report.applied_versions, vec![2, 3]);
        assert_eq!(affected_counts(&after), before_counts);
        assert_eq!(
            after
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let mut foreign_check = after.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(foreign_check.query([]).unwrap().next().unwrap().is_none());
        drop(foreign_check);
        let index_bytes: i64 = after
            .query_row(
                "SELECT COALESCE(SUM(pgsize), 0) FROM dbstat
                 WHERE name = 'idx_dictionary_scripture_refs_verse_id'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let plan: String = after
            .query_row(
                "EXPLAIN QUERY PLAN
                 SELECT entry_slug FROM dictionary_scripture_refs WHERE verse_id = 'GEN.1.1'",
                [],
                |row| row.get(3),
            )
            .unwrap();
        assert!(plan.contains("idx_dictionary_scripture_refs_verse_id"));
        let after_lookup_ms = dictionary_lookup_ms(&after, 500);
        let integrity_started = Instant::now();
        assert_eq!(
            after
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let integrity_ms = integrity_started.elapsed().as_millis();
        let after_bytes = fs::metadata(&path).unwrap().len();
        drop(after);

        let content_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/content-updates");
        let no_update_started = Instant::now();
        let first_startup =
            crate::content_updates::process_bundled_content_updates(&path, &content_root).unwrap();
        let first_startup_ms = no_update_started.elapsed().as_millis();
        assert_eq!(first_startup.content_version, 1);
        let repeated_started = Instant::now();
        let repeated_startup =
            crate::content_updates::process_bundled_content_updates(&path, &content_root).unwrap();
        let repeated_startup_ms = repeated_started.elapsed().as_millis();
        assert_eq!(repeated_startup.content_version, 1);
        eprintln!(
            "bundled copy={}ms backup={}ms migration={}ms integrity={}ms startup={}ms repeated={}ms before={} after={} delta={} index={} lookup_before={}ms lookup_after={}ms plan={}",
            copy_ms,
            backup_ms,
            migration_ms,
            integrity_ms,
            first_startup_ms,
            repeated_startup_ms,
            before_bytes,
            after_bytes,
            after_bytes as i128 - before_bytes as i128,
            index_bytes,
            before_lookup_ms,
            after_lookup_ms,
            plan
        );
    }
}
