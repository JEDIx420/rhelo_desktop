use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::Manager;
#[cfg(target_os = "windows")]
use tauri_plugin_shell::ShellExt;
use tts::Tts;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

mod biblical_correction;
mod research;

include!(concat!(env!("OUT_DIR"), "/schema_version.rs"));
const DATABASE_BACKUP_PREFIX: &str = "rhelo.backup-schema";

struct WhisperModelState {
    context: Mutex<Option<WhisperContext>>,
    initialization_error: Option<String>,
}

struct NativeTtsState {
    engine: Mutex<Option<Tts>>,
    initialization_error: Option<String>,
}

pub(crate) struct DatabaseState {
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct TtsVoiceDescriptor {
    name: String,
    language: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct TtsVoiceSelection {
    requested_language: String,
    normalized_locale: String,
    selected_voice: Option<TtsVoiceDescriptor>,
    available: bool,
    reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct TtsDiagnosticsResponse {
    os: String,
    native_tts_available: bool,
    initialization_error: Option<String>,
    current_schema_version: i32,
    detected_voices: Vec<TtsVoiceDescriptor>,
    english: TtsVoiceSelection,
    greek: TtsVoiceSelection,
    hebrew: TtsVoiceSelection,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct SystemSettingsOpenResult {
    opened: bool,
    requested_page: String,
    opened_uri: Option<String>,
    used_fallback: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Session {
    session_id: String,
    title: String,
    content: String,
    updated_at: String,
}

#[derive(Serialize)]
struct SessionsResponse {
    sessions: Vec<Session>,
}

#[derive(Serialize)]
struct CreateSessionResponse {
    status: &'static str,
    session_id: String,
    title: String,
    content: String,
}

#[derive(Serialize)]
struct UpdateSessionResponse {
    status: &'static str,
    session: Session,
}

#[derive(Serialize)]
struct DeleteSessionResponse {
    status: &'static str,
    session_id: String,
}

#[derive(Debug, Serialize)]
struct ChapterVerse {
    id: String,
    book: String,
    chapter: i64,
    verse: i64,
    text_en: String,
    text_original: String,
    text_hi: String,
    text_te: String,
    text_ml: String,
    text_ta: String,
    cross_references_count: i64,
    places_count: i64,
    commentaries: Vec<String>,
    morphology: serde_json::Value,
}

struct ChapterVerseRecord {
    id: String,
    book: String,
    chapter: i64,
    verse: i64,
    text_en: Option<String>,
    text_original: Option<String>,
    text_hi: Option<String>,
    text_te: Option<String>,
    text_ml: Option<String>,
    text_ta: Option<String>,
    cross_references_count: i64,
    places_count: i64,
    morphology: Option<String>,
}

#[derive(Serialize)]
struct ChapterResponse {
    verses: Vec<ChapterVerse>,
    translation_code: &'static str,
}

#[derive(Debug, Serialize, PartialEq)]
struct TranslationPassageVerse {
    verse: i64,
    text: String,
}

#[derive(Debug, Serialize)]
struct TranslationPassageResponse {
    translation_code: &'static str,
    book: String,
    chapter: i64,
    verses: Vec<TranslationPassageVerse>,
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

fn read_user_version(connection: &Connection) -> Result<i32, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("Failed to read SQLite user_version: {error}"))
}

fn backup_path_for_suffix(
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

fn create_database_backup(
    database_path: &Path,
    from_version: i32,
    to_version: i32,
) -> Result<PathBuf, String> {
    for suffix in 0.. {
        let backup_path = backup_path_for_suffix(database_path, from_version, to_version, suffix);
        let destination = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to reserve database backup path {:?}: {error}",
                    backup_path
                ))
            }
        };

        let mut source = fs::File::open(database_path).map_err(|error| {
            let _ = fs::remove_file(&backup_path);
            format!(
                "Failed to open the writable database for backup {:?}: {error}",
                database_path
            )
        })?;
        let mut destination = destination;
        if let Err(error) = io::copy(&mut source, &mut destination) {
            drop(destination);
            let _ = fs::remove_file(&backup_path);
            return Err(format!(
                "Failed to back up the writable database to {:?}: {error}",
                backup_path
            ));
        }
        if let Err(error) = destination.sync_all() {
            drop(destination);
            let _ = fs::remove_file(&backup_path);
            return Err(format!(
                "Failed to flush database backup {:?}: {error}",
                backup_path
            ));
        }
        return Ok(backup_path);
    }

    unreachable!("the collision-safe backup suffix space is unbounded")
}

type MigrationFn = fn(&Transaction<'_>) -> Result<(), String>;

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

fn ordered_migrations() -> &'static [(i32, MigrationFn)] {
    &[(1, migration_001_baseline)]
}

fn apply_migrations(connection: &mut Connection, from_version: i32) -> Result<(), String> {
    for (version, migration) in ordered_migrations() {
        if *version <= from_version {
            continue;
        }

        let transaction = connection
            .transaction()
            .map_err(|error| format!("Failed to start migration transaction {version}: {error}"))?;
        migration(&transaction)?;
        transaction
            .execute_batch(&format!("PRAGMA user_version = {version};"))
            .map_err(|error| {
                format!("Migration {version} failed while updating user_version: {error}")
            })?;
        transaction
            .commit()
            .map_err(|error| format!("Failed to commit migration {version}: {error}"))?;
    }
    Ok(())
}

fn ensure_database_schema(database_path: &Path) -> Result<(), String> {
    let mut connection = open_database(database_path)?;
    let user_version = read_user_version(&connection)?;

    if user_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "The writable database schema version ({user_version}) is newer than this app supports ({CURRENT_SCHEMA_VERSION})."
        ));
    }

    if user_version == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    let backup_path = create_database_backup(database_path, user_version, CURRENT_SCHEMA_VERSION)?;
    println!(
        "Running database migrations from schema v{} to v{} using backup {:?}",
        user_version, CURRENT_SCHEMA_VERSION, backup_path
    );

    apply_migrations(&mut connection, user_version).map_err(|error| {
        format!(
            "Database migration failed at schema version {user_version}. Backup preserved at {:?}. {error}",
            backup_path
        )
    })
}

fn read_session(connection: &Connection, session_id: &str) -> Result<Option<Session>, String> {
    connection
        .query_row(
            "SELECT session_id, title, content, updated_at FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok(Session {
                    session_id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("Failed to read the study session: {error}"))
}

pub(crate) fn normalize_english_translation(translation_code: Option<&str>) -> &'static str {
    match translation_code {
        Some("en_web") => "en_web",
        Some("en_kjv") => "en_kjv",
        _ => "en_bsb",
    }
}

fn validate_embedded_translation(translation_code: &str) -> Result<&'static str, String> {
    match translation_code {
        "en_bsb" => Ok("en_bsb"),
        "en_web" => Ok("en_web"),
        "en_kjv" => Ok("en_kjv"),
        _ => Err("The requested embedded translation is not available.".to_string()),
    }
}

fn query_translation_passage(
    connection: &Connection,
    translation_code: &'static str,
    book: &str,
    chapter: i64,
) -> Result<Vec<TranslationPassageVerse>, String> {
    let mut statement = connection
        .prepare(
            "SELECT vb.verse, COALESCE(selected.text, kjv.text, legacy.text)
             FROM verses_base vb
             LEFT JOIN verse_translations selected
               ON selected.verse_id = vb.id AND selected.translation_code = ?1
             LEFT JOIN verse_translations kjv
               ON kjv.verse_id = vb.id AND kjv.translation_code = 'en_kjv'
             LEFT JOIN verse_translations legacy
               ON legacy.verse_id = vb.id AND legacy.translation_code = 'en'
             WHERE vb.book = ?2 AND vb.chapter = ?3
             ORDER BY vb.verse",
        )
        .map_err(|error| format!("Failed to prepare the translation passage query: {error}"))?;

    let verses = statement
        .query_map(params![translation_code, book, chapter], |row| {
            Ok(TranslationPassageVerse {
                verse: row.get(0)?,
                text: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        })
        .map_err(|error| format!("Failed to read {book} {chapter}: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Failed to decode {book} {chapter}: {error}"))?;
    Ok(verses)
}

#[tauri::command]
fn fetch_translation_passage(
    book: String,
    chapter: i64,
    translation_code: String,
    state: tauri::State<'_, DatabaseState>,
) -> Result<TranslationPassageResponse, String> {
    let book = book.trim().to_ascii_uppercase();
    if book.is_empty() {
        return Err("A Scripture book code is required.".to_string());
    }
    if chapter < 1 {
        return Err("The chapter number must be greater than zero.".to_string());
    }
    let translation_code = validate_embedded_translation(translation_code.trim())?;
    let connection = open_database(&state.path)?;
    let verses = query_translation_passage(&connection, translation_code, &book, chapter)?;
    if verses.is_empty() {
        return Err(format!(
            "No embedded passage was found for {book} {chapter}."
        ));
    }

    Ok(TranslationPassageResponse {
        translation_code,
        book,
        chapter,
        verses,
    })
}

#[tauri::command]
fn fetch_chapter(
    book: String,
    chapter: i64,
    translation_code: Option<String>,
    state: tauri::State<'_, DatabaseState>,
) -> Result<ChapterResponse, String> {
    let book = book.trim().to_ascii_uppercase();
    if book.is_empty() {
        return Err("A Scripture book code is required.".to_string());
    }
    if chapter < 1 {
        return Err("The chapter number must be greater than zero.".to_string());
    }

    let translation_code = normalize_english_translation(translation_code.as_deref());
    let connection = open_database(&state.path)?;
    let mut statement = connection
        .prepare(
            "SELECT
                v.id,
                v.book,
                v.chapter,
                v.verse,
                COALESCE(et.text, v.text_en) AS active_text_en,
                v.text_original,
                v.text_hi,
                v.text_te,
                v.text_ml,
                v.text_ta,
                (SELECT COUNT(*) FROM cross_references cr WHERE cr.from_verse = v.id),
                (SELECT COUNT(*) FROM verse_geography vg WHERE vg.verse_id = v.id),
                v.morphology
             FROM verses v
             LEFT JOIN verse_translations et
               ON et.verse_id = v.id AND et.translation_code = ?1
             WHERE v.book = ?2 AND v.chapter = ?3
             ORDER BY v.verse",
        )
        .map_err(|error| format!("Failed to prepare the chapter query: {error}"))?;

    let records = statement
        .query_map(params![translation_code, book, chapter], |row| {
            Ok(ChapterVerseRecord {
                id: row.get(0)?,
                book: row.get(1)?,
                chapter: row.get(2)?,
                verse: row.get(3)?,
                text_en: row.get(4)?,
                text_original: row.get(5)?,
                text_hi: row.get(6)?,
                text_te: row.get(7)?,
                text_ml: row.get(8)?,
                text_ta: row.get(9)?,
                cross_references_count: row.get(10)?,
                places_count: row.get(11)?,
                morphology: row.get(12)?,
            })
        })
        .map_err(|error| format!("Failed to read {book} {chapter}: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Failed to decode {book} {chapter}: {error}"))?;
    drop(statement);

    let mut commentary_statement = connection
        .prepare("SELECT text FROM commentaries WHERE verse_id = ?1")
        .map_err(|error| format!("Failed to prepare the commentary query: {error}"))?;
    let mut verses = Vec::with_capacity(records.len());

    for record in records {
        let commentaries = commentary_statement
            .query_map([&record.id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Failed to read commentaries for {}: {error}", record.id))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Failed to decode commentaries for {}: {error}", record.id))?;
        let morphology = record
            .morphology
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .filter(serde_json::Value::is_array)
            .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));

        verses.push(ChapterVerse {
            id: record.id,
            book: record.book,
            chapter: record.chapter,
            verse: record.verse,
            text_en: record.text_en.unwrap_or_default(),
            text_original: record.text_original.unwrap_or_default(),
            text_hi: record.text_hi.unwrap_or_default(),
            text_te: record.text_te.unwrap_or_default(),
            text_ml: record.text_ml.unwrap_or_default(),
            text_ta: record.text_ta.unwrap_or_default(),
            cross_references_count: record.cross_references_count,
            places_count: record.places_count,
            commentaries,
            morphology,
        });
    }

    Ok(ChapterResponse {
        verses,
        translation_code,
    })
}

#[tauri::command]
fn fetch_sessions(state: tauri::State<'_, DatabaseState>) -> Result<SessionsResponse, String> {
    let connection = open_database(&state.path)?;
    let mut statement = connection
        .prepare(
            "SELECT session_id, title, content, updated_at FROM sessions ORDER BY updated_at DESC",
        )
        .map_err(|error| format!("Failed to prepare the sessions query: {error}"))?;
    let sessions = statement
        .query_map([], |row| {
            Ok(Session {
                session_id: row.get(0)?,
                title: row.get(1)?,
                content: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })
        .map_err(|error| format!("Failed to fetch study sessions: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Failed to decode study sessions: {error}"))?;
    Ok(SessionsResponse { sessions })
}

#[tauri::command]
fn create_session(
    title: String,
    content: String,
    state: tauri::State<'_, DatabaseState>,
) -> Result<CreateSessionResponse, String> {
    let mut connection = open_database(&state.path)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Failed to start the create-session transaction: {error}"))?;
    let session_id = uuid::Uuid::new_v4().to_string();
    transaction
        .execute(
            "INSERT INTO sessions (session_id, title, content) VALUES (?1, ?2, ?3)",
            params![session_id, title, content],
        )
        .map_err(|error| format!("Failed to create the study session: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Failed to commit the study session: {error}"))?;
    Ok(CreateSessionResponse {
        status: "success",
        session_id,
        title,
        content,
    })
}

#[tauri::command]
fn update_session(
    session_id: String,
    title: String,
    content: String,
    state: tauri::State<'_, DatabaseState>,
) -> Result<UpdateSessionResponse, String> {
    let mut connection = open_database(&state.path)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Failed to start the update-session transaction: {error}"))?;
    let changed = transaction
        .execute(
            "UPDATE sessions SET title = ?1, content = ?2, updated_at = CURRENT_TIMESTAMP WHERE session_id = ?3",
            params![title, content, session_id],
        )
        .map_err(|error| format!("Failed to update the study session: {error}"))?;
    if changed == 0 {
        return Err("The selected study session no longer exists.".to_string());
    }
    transaction
        .commit()
        .map_err(|error| format!("Failed to commit the study session: {error}"))?;
    let session = read_session(&connection, &session_id)?
        .ok_or_else(|| "The updated study session could not be reloaded.".to_string())?;
    Ok(UpdateSessionResponse {
        status: "success",
        session,
    })
}

#[tauri::command]
fn delete_session(
    session_id: String,
    state: tauri::State<'_, DatabaseState>,
) -> Result<DeleteSessionResponse, String> {
    let mut connection = open_database(&state.path)?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Failed to start the delete-session transaction: {error}"))?;
    let changed = transaction
        .execute("DELETE FROM sessions WHERE session_id = ?1", [&session_id])
        .map_err(|error| format!("Failed to delete the study session: {error}"))?;
    if changed == 0 {
        return Err("The selected study session no longer exists.".to_string());
    }
    transaction
        .commit()
        .map_err(|error| format!("Failed to commit the session deletion: {error}"))?;
    Ok(DeleteSessionResponse {
        status: "success",
        session_id,
    })
}

fn prepare_database(app: &mut tauri::App) -> Result<PathBuf, String> {
    // 1. Resolve source and destination DB paths
    let db_src = app
        .path()
        .resolve("rhelo.db", tauri::path::BaseDirectory::Resource)
        .map_err(|e| format!("Failed to resolve source DB path: {}", e))?;

    let db_dest = app
        .path()
        .resolve("rhelo.db", tauri::path::BaseDirectory::AppData)
        .map_err(|e| format!("Failed to resolve destination DB path: {}", e))?;

    prepare_database_at_paths(&db_src, &db_dest)
}

fn prepare_database_at_paths(db_src: &Path, db_dest: &Path) -> Result<PathBuf, String> {
    // Copying a current seed is a fresh install, so schema validation is a no-op.
    if !db_dest.exists() {
        if let Some(parent) = db_dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create AppData directories: {}", e))?;
        }
        fs::copy(db_src, db_dest).map_err(|e| {
            format!(
                "Failed to copy DB from {:?} to {:?}: {}",
                db_src, db_dest, e
            )
        })?;
    }

    ensure_database_schema(db_dest)?;

    Ok(db_dest.to_path_buf())
}

fn normalize_language_tag(language: &str) -> String {
    language.trim().to_lowercase().replace('_', "-")
}

fn base_language(language: &str) -> String {
    normalize_language_tag(language)
        .split('-')
        .next()
        .unwrap_or("")
        .to_string()
}

fn normalize_requested_tts_locale(language: &str) -> Result<String, String> {
    match normalize_language_tag(language).as_str() {
        "en" | "en-us" | "en-gb" => Ok("en-us".to_string()),
        "el" | "el-gr" => Ok("el-gr".to_string()),
        "he" | "he-il" | "iw" | "iw-il" => Ok("he-il".to_string()),
        _ => Err(
            "[tts:unsupported-language] Rhelo TTS supports English, Hebrew, and Greek only."
                .to_string(),
        ),
    }
}

fn locale_aliases(language: &str) -> Vec<String> {
    match normalize_requested_tts_locale(language).as_deref() {
        Ok("en-us") => vec!["en-us".to_string(), "en-gb".to_string(), "en".to_string()],
        Ok("el-gr") => vec!["el-gr".to_string(), "el".to_string()],
        Ok("he-il") => vec![
            "he-il".to_string(),
            "he".to_string(),
            "iw-il".to_string(),
            "iw".to_string(),
        ],
        _ => Vec::new(),
    }
}

fn voice_name_hints_for(language: &str) -> &'static [&'static str] {
    match normalize_requested_tts_locale(language).as_deref() {
        Ok("el-gr") => &["greek", "stefanos"],
        Ok("he-il") => &["hebrew"],
        Ok("en-us") => &["english"],
        _ => &[],
    }
}

fn build_voice_catalog(tts: &mut Tts) -> Result<Vec<TtsVoiceDescriptor>, String> {
    tts.voices()
        .map_err(|error| {
            format!("[tts:native-query-failed] Failed to query installed TTS voices: {error}")
        })?
        .into_iter()
        .map(|voice| {
            Ok(TtsVoiceDescriptor {
                name: voice.name().to_string(),
                language: normalize_language_tag(&voice.language()),
            })
        })
        .collect()
}

fn select_voice(
    voices: &[TtsVoiceDescriptor],
    target_language: &str,
) -> Option<TtsVoiceDescriptor> {
    let normalized_target = normalize_requested_tts_locale(target_language).ok()?;
    let target_base = base_language(&normalized_target);
    let aliases = locale_aliases(&normalized_target);

    voices
        .iter()
        .find(|voice| normalize_language_tag(&voice.language) == normalized_target)
        .cloned()
        .or_else(|| {
            voices
                .iter()
                .find(|voice| base_language(&voice.language) == target_base)
                .cloned()
        })
        .or_else(|| {
            voices
                .iter()
                .find(|voice| {
                    aliases
                        .iter()
                        .any(|alias| normalize_language_tag(&voice.language) == *alias)
                })
                .cloned()
        })
        .or_else(|| {
            let hints = voice_name_hints_for(&normalized_target);
            voices
                .iter()
                .find(|voice| {
                    let normalized_voice_language = normalize_language_tag(&voice.language);
                    base_language(&normalized_voice_language) == target_base
                        && hints
                            .iter()
                            .any(|hint| voice.name.to_lowercase().contains(hint))
                })
                .cloned()
        })
}

fn selection_for_language(
    voices: &[TtsVoiceDescriptor],
    language: &str,
) -> Result<TtsVoiceSelection, String> {
    let normalized_locale = normalize_requested_tts_locale(language)?;
    let selected_voice = select_voice(voices, &normalized_locale);
    let reason = if selected_voice.is_none() {
        Some(format!(
            "No compatible voice is installed for {}.",
            normalized_locale
        ))
    } else {
        None
    };

    Ok(TtsVoiceSelection {
        requested_language: language.to_string(),
        normalized_locale,
        available: selected_voice.is_some(),
        selected_voice,
        reason,
    })
}

#[tauri::command]
fn fetch_tts_diagnostics(
    tts_state: tauri::State<'_, NativeTtsState>,
) -> Result<TtsDiagnosticsResponse, String> {
    let mut guard = tts_state
        .engine
        .lock()
        .map_err(|_| "[tts:state-lock-failed] Failed to lock native TTS state.".to_string())?;
    let detected_voices = if let Some(engine) = guard.as_mut() {
        build_voice_catalog(engine)?
    } else {
        Vec::new()
    };

    Ok(TtsDiagnosticsResponse {
        os: std::env::consts::OS.to_string(),
        native_tts_available: guard.is_some(),
        initialization_error: tts_state.initialization_error.clone(),
        current_schema_version: CURRENT_SCHEMA_VERSION,
        english: selection_for_language(&detected_voices, "en")?,
        greek: selection_for_language(&detected_voices, "el")?,
        hebrew: selection_for_language(&detected_voices, "he")?,
        detected_voices,
    })
}

fn windows_settings_uri(page: &str) -> Option<&'static str> {
    match page {
        "language" => Some("ms-settings:regionlanguage"),
        "speech" => Some("ms-settings:speech"),
        _ => None,
    }
}

#[tauri::command]
fn open_windows_settings(page: String, app: tauri::AppHandle) -> SystemSettingsOpenResult {
    let Some(uri) = windows_settings_uri(&page) else {
        return SystemSettingsOpenResult {
            opened: false,
            requested_page: page,
            opened_uri: None,
            used_fallback: false,
            error: Some("Unsupported Windows Settings page.".to_string()),
        };
    };

    #[cfg(target_os = "windows")]
    {
        #[allow(deprecated)]
        match app.shell().open(uri, None) {
            Ok(()) => SystemSettingsOpenResult {
                opened: true,
                requested_page: page,
                opened_uri: Some(uri.to_string()),
                used_fallback: false,
                error: None,
            },
            Err(primary_error) => {
                let fallback_uri = "ms-settings:";
                #[allow(deprecated)]
                match app.shell().open(fallback_uri, None) {
                    Ok(()) => SystemSettingsOpenResult {
                        opened: true,
                        requested_page: page,
                        opened_uri: Some(fallback_uri.to_string()),
                        used_fallback: true,
                        error: Some(format!(
                            "The requested page could not be opened directly: {primary_error}"
                        )),
                    },
                    Err(fallback_error) => SystemSettingsOpenResult {
                        opened: false,
                        requested_page: page,
                        opened_uri: None,
                        used_fallback: true,
                        error: Some(format!(
                            "Windows Settings could not be opened: {primary_error}; fallback failed: {fallback_error}"
                        )),
                    },
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        SystemSettingsOpenResult {
            opened: false,
            requested_page: page,
            opened_uri: None,
            used_fallback: false,
            error: Some(format!(
                "Windows Settings URI {uri} is unavailable on this operating system."
            )),
        }
    }
}

#[tauri::command]
fn speak_text(
    text: String,
    lang: String,
    tts_state: tauri::State<'_, NativeTtsState>,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("[tts:empty-text] Rhelo cannot speak an empty text selection.".to_string());
    }

    let normalized_locale = normalize_requested_tts_locale(&lang)?;
    let mut guard = tts_state
        .engine
        .lock()
        .map_err(|_| "[tts:state-lock-failed] Failed to lock native TTS state.".to_string())?;
    let tts = guard.as_mut().ok_or_else(|| {
        let detail = tts_state
            .initialization_error
            .clone()
            .unwrap_or_else(|| "Native TTS is unavailable on this system.".to_string());
        format!("[tts:native-unavailable] {detail}")
    })?;
    let voices = build_voice_catalog(tts)?;
    let selected_voice = select_voice(&voices, &normalized_locale).ok_or_else(|| {
        let available_languages = voices
            .iter()
            .map(|voice| voice.language.clone())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[tts:voice-unavailable] No compatible voice is installed for {}. Available voice locales: {}.",
            normalized_locale, available_languages
        )
    })?;

    let native_voices = tts.voices().map_err(|error| {
        format!("[tts:native-query-failed] Failed to query installed TTS voices: {error}")
    })?;
    let native_voice = native_voices
        .iter()
        .find(|voice| {
            voice.name() == selected_voice.name
                && normalize_language_tag(&voice.language()) == selected_voice.language
        })
        .ok_or_else(|| {
            format!(
                "[tts:voice-selection-failed] The selected voice '{}' ({}) could not be reopened.",
                selected_voice.name, selected_voice.language
            )
        })?;

    println!(
        "TTS request: os={} requested_lang={} normalized_locale={} chars={} selected_voice={} selected_locale={}",
        std::env::consts::OS,
        lang,
        normalized_locale,
        text.chars().count(),
        selected_voice.name,
        selected_voice.language
    );

    tts.set_voice(native_voice).map_err(|error| {
        format!(
            "[tts:voice-selection-failed] Failed to select '{}' for '{}': {error}",
            selected_voice.name, normalized_locale
        )
    })?;

    tts.speak(text, true)
        .map_err(|error| format!("[tts:speak-failed] Speech synthesis failed: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tts_voice_tests {
    use super::{
        apply_migrations, backup_path_for_suffix, normalize_requested_tts_locale, open_database,
        prepare_database_at_paths, query_translation_passage, read_user_version, select_voice,
        validate_embedded_translation, windows_settings_uri, TtsVoiceDescriptor,
        CURRENT_SCHEMA_VERSION, DATABASE_BACKUP_PREFIX,
    };
    use rusqlite::Connection;
    use std::fs;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn voice(name: &str, language: &str) -> TtsVoiceDescriptor {
        TtsVoiceDescriptor {
            name: name.to_string(),
            language: language.to_string(),
        }
    }

    #[test]
    fn embedded_translation_allowlist_rejects_provider_ids_from_the_ui() {
        assert_eq!(validate_embedded_translation("en_bsb"), Ok("en_bsb"));
        assert_eq!(validate_embedded_translation("en_web"), Ok("en_web"));
        assert_eq!(validate_embedded_translation("en_kjv"), Ok("en_kjv"));
        assert!(validate_embedded_translation("display name").is_err());
        assert!(validate_embedded_translation("remote-provider-id").is_err());
    }

    #[test]
    fn embedded_passage_query_uses_selected_text_and_canonical_fallback() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE verses_base (
                    id TEXT PRIMARY KEY, book TEXT, chapter INTEGER, verse INTEGER
                 );
                 CREATE TABLE verse_translations (
                    verse_id TEXT, translation_code TEXT, text TEXT,
                    PRIMARY KEY (verse_id, translation_code)
                 );
                 INSERT INTO verses_base VALUES ('JHN.3.1', 'JHN', 3, 1), ('JHN.3.2', 'JHN', 3, 2);
                 INSERT INTO verse_translations VALUES
                    ('JHN.3.1', 'en_web', 'WEB one'),
                    ('JHN.3.1', 'en_kjv', 'KJV one'),
                    ('JHN.3.2', 'en_kjv', 'KJV fallback'),
                    ('JHN.3.2', 'en', 'Legacy two');",
            )
            .unwrap();

        let passage = query_translation_passage(&connection, "en_web", "JHN", 3).unwrap();
        assert_eq!(passage[0].text, "WEB one");
        assert_eq!(passage[1].text, "KJV fallback");
    }

    #[test]
    fn maps_supported_languages_to_canonical_locales() {
        assert_eq!(
            normalize_requested_tts_locale("en"),
            Ok("en-us".to_string())
        );
        assert_eq!(
            normalize_requested_tts_locale("en-GB"),
            Ok("en-us".to_string())
        );
        assert_eq!(
            normalize_requested_tts_locale("HE_il"),
            Ok("he-il".to_string())
        );
        assert_eq!(
            normalize_requested_tts_locale("iw-IL"),
            Ok("he-il".to_string())
        );
        assert_eq!(
            normalize_requested_tts_locale("el-GR"),
            Ok("el-gr".to_string())
        );
        assert!(normalize_requested_tts_locale("fr-FR").is_err());
    }

    #[test]
    fn prefers_an_exact_locale_match() {
        let voices = vec![
            voice("British English", "en-gb"),
            voice("American English", "en-us"),
        ];
        assert_eq!(
            select_voice(&voices, "en-us"),
            Some(voice("American English", "en-us"))
        );
    }

    #[test]
    fn prefers_same_language_over_name_only_hints() {
        let voices = vec![
            voice("Microsoft Stefanos Greek", "en-us"),
            voice("Greek Polytonic", "el"),
        ];
        assert_eq!(
            select_voice(&voices, "el-gr"),
            Some(voice("Greek Polytonic", "el"))
        );
    }

    #[test]
    fn supports_legacy_hebrew_locale_aliases() {
        let voices = vec![voice("Hebrew Voice", "iw-il")];
        assert_eq!(
            select_voice(&voices, "he-il"),
            Some(voice("Hebrew Voice", "iw-il"))
        );
    }

    #[test]
    fn never_uses_english_voice_for_greek_or_hebrew() {
        let english_only = vec![voice("English Only", "en-us")];
        assert_eq!(select_voice(&english_only, "el-gr"), None);
        assert_eq!(select_voice(&english_only, "he-il"), None);
    }

    #[test]
    fn windows_voice_setup_uses_restricted_settings_uris() {
        assert_eq!(
            windows_settings_uri("language"),
            Some("ms-settings:regionlanguage")
        );
        assert_eq!(windows_settings_uri("speech"), Some("ms-settings:speech"));
        assert_eq!(windows_settings_uri("arbitrary"), None);
    }

    fn temp_db_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rhelo-tests-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn create_db(path: &Path) -> Connection {
        Connection::open(path).unwrap()
    }

    fn create_session_schema(connection: &Connection) {
        connection
            .execute_batch(
                "
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
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                CREATE VIRTUAL TABLE sessions_fts USING fts5(
                    session_id UNINDEXED,
                    title,
                    content
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
                ",
            )
            .unwrap();
    }

    #[test]
    fn backup_name_has_schema_metadata() {
        let path = PathBuf::from("/tmp/rhelo.db");
        let backup = backup_path_for_suffix(&path, 0, CURRENT_SCHEMA_VERSION, 0);
        let name = backup.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains(DATABASE_BACKUP_PREFIX));
        assert!(name.contains("v0"));
    }

    #[test]
    fn migrates_version_zero_database_to_current() {
        let db_path = temp_db_path("migrate.sqlite3");
        let connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        super::ensure_database_schema(&db_path).unwrap();

        let connection = open_database(&db_path).unwrap();
        assert_eq!(
            read_user_version(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn already_current_database_is_left_alone() {
        let db_path = temp_db_path("current.sqlite3");
        let connection = create_db(&db_path);
        connection
            .execute_batch(&format!(
                "PRAGMA user_version = {};",
                CURRENT_SCHEMA_VERSION
            ))
            .unwrap();
        drop(connection);

        super::ensure_database_schema(&db_path).unwrap();

        let backup_dir = db_path.parent().unwrap();
        let backups = fs::read_dir(backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(DATABASE_BACKUP_PREFIX)
            })
            .count();
        assert_eq!(backups, 0);
    }

    #[test]
    fn fresh_install_copies_current_seed_without_backup() {
        let seed_path = temp_db_path("seed.sqlite3");
        let app_data_path = seed_path.parent().unwrap().join("app-data/rhelo.db");
        let seed = create_db(&seed_path);
        create_session_schema(&seed);
        seed.execute_batch(&format!(
            "PRAGMA user_version = {};",
            CURRENT_SCHEMA_VERSION
        ))
        .unwrap();
        drop(seed);

        prepare_database_at_paths(&seed_path, &app_data_path).unwrap();

        let installed = open_database(&app_data_path).unwrap();
        assert_eq!(
            read_user_version(&installed).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        let backup_count = fs::read_dir(app_data_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(DATABASE_BACKUP_PREFIX)
            })
            .count();
        assert_eq!(backup_count, 0);
    }

    #[test]
    fn existing_current_install_is_not_replaced_or_backed_up() {
        let seed_path = temp_db_path("seed.sqlite3");
        let app_data_path = seed_path.parent().unwrap().join("app-data/rhelo.db");
        fs::create_dir_all(app_data_path.parent().unwrap()).unwrap();

        let seed = create_db(&seed_path);
        seed.execute_batch(&format!(
            "PRAGMA user_version = {};",
            CURRENT_SCHEMA_VERSION
        ))
        .unwrap();
        drop(seed);

        let installed = create_db(&app_data_path);
        create_session_schema(&installed);
        installed
            .execute(
                "INSERT INTO sessions (session_id, title, content) VALUES ('existing', 'Keep me', 'Body')",
                [],
            )
            .unwrap();
        installed
            .execute_batch(&format!(
                "PRAGMA user_version = {};",
                CURRENT_SCHEMA_VERSION
            ))
            .unwrap();
        drop(installed);

        prepare_database_at_paths(&seed_path, &app_data_path).unwrap();

        let installed = open_database(&app_data_path).unwrap();
        let title: String = installed
            .query_row(
                "SELECT title FROM sessions WHERE session_id = 'existing'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Keep me");
        assert!(!backup_path_for_suffix(&app_data_path, 0, 1, 0).exists());
    }

    #[test]
    fn existing_old_install_is_backed_up_migrated_and_preserved() {
        let seed_path = temp_db_path("seed.sqlite3");
        let app_data_path = seed_path.parent().unwrap().join("app-data/rhelo.db");
        fs::create_dir_all(app_data_path.parent().unwrap()).unwrap();

        let seed = create_db(&seed_path);
        seed.execute_batch(&format!(
            "PRAGMA user_version = {};",
            CURRENT_SCHEMA_VERSION
        ))
        .unwrap();
        drop(seed);

        let installed = create_db(&app_data_path);
        create_session_schema(&installed);
        installed
            .execute(
                "INSERT INTO sessions (session_id, title, content) VALUES ('upgrade', 'Survives', 'Body')",
                [],
            )
            .unwrap();
        installed.execute_batch("PRAGMA user_version = 0;").unwrap();
        drop(installed);

        prepare_database_at_paths(&seed_path, &app_data_path).unwrap();

        let installed = open_database(&app_data_path).unwrap();
        assert_eq!(
            read_user_version(&installed).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        let title: String = installed
            .query_row(
                "SELECT title FROM sessions WHERE session_id = 'upgrade'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Survives");
        assert!(backup_path_for_suffix(&app_data_path, 0, CURRENT_SCHEMA_VERSION, 0).exists());
    }

    #[test]
    fn migration_is_effectively_idempotent() {
        let db_path = temp_db_path("idempotent.sqlite3");
        let connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        super::ensure_database_schema(&db_path).unwrap();
        super::ensure_database_schema(&db_path).unwrap();

        let connection = open_database(&db_path).unwrap();
        assert_eq!(
            read_user_version(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }

    #[test]
    fn existing_sessions_are_preserved() {
        let db_path = temp_db_path("sessions.sqlite3");
        let connection = create_db(&db_path);
        create_session_schema(&connection);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions (session_id, title, content) VALUES (?1, ?2, ?3)",
                ["session-1", "Title", "Body"],
            )
            .unwrap();
        drop(connection);

        super::ensure_database_schema(&db_path).unwrap();

        let connection = open_database(&db_path).unwrap();
        let title: String = connection
            .query_row(
                "SELECT title FROM sessions WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(title, "Title");
    }

    #[test]
    fn backup_is_created_before_migration() {
        let db_path = temp_db_path("backup.sqlite3");
        let connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        super::ensure_database_schema(&db_path).unwrap();

        let backup_dir = db_path.parent().unwrap();
        let backups = fs::read_dir(backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(DATABASE_BACKUP_PREFIX)
            })
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn existing_backup_is_preserved_and_retry_uses_a_suffix() {
        let db_path = temp_db_path("collision.sqlite3");
        let connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        drop(connection);

        let first_backup = backup_path_for_suffix(&db_path, 0, CURRENT_SCHEMA_VERSION, 0);
        fs::write(&first_backup, b"preserve this backup").unwrap();

        super::ensure_database_schema(&db_path).unwrap();

        assert_eq!(fs::read(&first_backup).unwrap(), b"preserve this backup");
        assert!(backup_path_for_suffix(&db_path, 0, CURRENT_SCHEMA_VERSION, 1).exists());
    }

    #[test]
    fn corrupted_database_returns_an_error() {
        let db_path = temp_db_path("corrupt.sqlite3");
        fs::write(&db_path, "not-a-sqlite-db").unwrap();
        let error = super::ensure_database_schema(&db_path).unwrap_err();
        assert!(error.contains("Failed to open the Rhelo database") || error.contains("SQLite"));
    }

    #[test]
    fn failing_migration_rolls_back_user_version() {
        fn failing_migration(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
            transaction
                .execute_batch("CREATE TABLE rollback_probe (id INTEGER PRIMARY KEY);")
                .map_err(|error| error.to_string())?;
            Err("boom".to_string())
        }

        let db_path = temp_db_path("rollback.sqlite3");
        let mut connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();

        let transaction = connection.transaction().unwrap();
        let result = failing_migration(&transaction).and_then(|_| {
            transaction
                .execute_batch("PRAGMA user_version = 1;")
                .map_err(|error| error.to_string())
        });
        assert!(result.is_err());
        drop(transaction);

        let connection = open_database(&db_path).unwrap();
        assert_eq!(read_user_version(&connection).unwrap(), 0);
    }

    #[test]
    fn ordered_migrations_can_run_directly() {
        let db_path = temp_db_path("apply.sqlite3");
        let mut connection = create_db(&db_path);
        connection
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();
        apply_migrations(&mut connection, 0).unwrap();
        let connection = open_database(&db_path).unwrap();
        assert_eq!(
            read_user_version(&connection).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }
}

#[tauri::command]
fn stop_speech(tts_state: tauri::State<'_, NativeTtsState>) -> Result<(), String> {
    let mut guard = tts_state
        .engine
        .lock()
        .map_err(|_| "[tts:state-lock-failed] Failed to lock native TTS state.".to_string())?;
    let tts = guard.as_mut().ok_or_else(|| {
        "[tts:native-unavailable] Native TTS is unavailable on this system.".to_string()
    })?;
    tts.stop()
        .map_err(|error| format!("[tts:stop-failed] Failed to stop speech: {error}"))?;
    Ok(())
}

#[tauri::command]
async fn transcribe_audio(
    audio_samples: Vec<f32>,
    model_state: tauri::State<'_, WhisperModelState>,
) -> Result<String, String> {
    let ctx_guard = model_state
        .context
        .lock()
        .map_err(|_| "Failed to lock Whisper context")?;
    let ctx = ctx_guard.as_ref().ok_or_else(|| {
        model_state.initialization_error.clone().unwrap_or_else(|| {
            "Whisper model is not initialized. Please ensure ggml-base.bin is in resources."
                .to_string()
        })
    })?;

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create Whisper state: {:?}", e))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    params.set_language(Some("auto"));
    params.set_n_threads(4);
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, &audio_samples[..])
        .map_err(|e| format!("Whisper execution failed: {:?}", e))?;

    let mut result = String::new();
    let num_segments = state
        .full_n_segments()
        .map_err(|e| format!("Failed to get segment count: {:?}", e))?;
    for i in 0..num_segments {
        if let Ok(text) = state.full_get_segment_text(i) {
            result.push_str(&text);
        }
    }

    Ok(result.trim().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if cfg!(debug_assertions) {
                if let Err(err) = app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                ) {
                    eprintln!("BOOT DIAGNOSTIC: failed to initialize logging: {err:?}");
                }
            }

            let database_path = prepare_database(app).map_err(|error| {
                eprintln!("CRITICAL BOOT ERROR: {error:?}");
                if let Ok(mut home) = app.path().home_dir() {
                    home.push("rhelo_boot_error.log");
                    let log_content = format!("Boot Error: {error}\n");
                    let _ = std::fs::write(home, log_content);
                }
                std::io::Error::other(error)
            })?;
            app.manage(DatabaseState {
                path: database_path,
            });
            app.manage(biblical_correction::BiblicalVocabularyState::default());
            if let Ok(mut home) = app.path().home_dir() {
                home.push("rhelo_boot_error.log");
                let _ = std::fs::remove_file(home);
            }

            // 1. Initialize native TTS and manage it in Tauri State
            let native_tts_state = match Tts::default() {
                Ok(tts_instance) => NativeTtsState {
                    engine: Mutex::new(Some(tts_instance)),
                    initialization_error: None,
                },
                Err(err) => {
                    eprintln!("BOOT DIAGNOSTIC: failed to initialize TTS: {err:?}");
                    NativeTtsState {
                        engine: Mutex::new(None),
                        initialization_error: Some(err.to_string()),
                    }
                }
            };
            app.manage(native_tts_state);

            // 2. Resolve and load Whisper model context
            let model_path = app
                .path()
                .resolve("ggml-base.bin", tauri::path::BaseDirectory::Resource);
            let (whisper_ctx, initialization_error) = match model_path {
                Ok(path) if path.exists() => match WhisperContext::new_with_params(
                    &path.to_string_lossy(),
                    WhisperContextParameters::default(),
                ) {
                    Ok(context) => (Some(context), None),
                    Err(error) => (
                        None,
                        Some(format!(
                            "Failed to load Whisper model at {:?}: {:?}",
                            path, error
                        )),
                    ),
                },
                Ok(path) => (
                    None,
                    Some(format!(
                        "Whisper model resource was not found at {:?}",
                        path
                    )),
                ),
                Err(error) => (
                    None,
                    Some(format!(
                        "Failed to resolve Whisper model resource: {}",
                        error
                    )),
                ),
            };

            app.manage(WhisperModelState {
                context: Mutex::new(whisper_ctx),
                initialization_error,
            });

            if let Some(window) = app.get_webview_window("main") {
                if let Err(err) = window.show() {
                    eprintln!("BOOT DIAGNOSTIC: failed to show the main window: {err:?}");
                }
                if let Err(err) = window.set_focus() {
                    eprintln!("BOOT DIAGNOSTIC: failed to focus the main window: {err:?}");
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            fetch_chapter,
            fetch_translation_passage,
            research::search_scripture,
            research::lookup_lexicon,
            research::fetch_research_meta,
            research::fetch_dictionary_study,
            research::fetch_verse_details,
            research::fetch_chapter_map,
            research::fetch_geography_routes,
            research::fetch_route_points,
            research::fetch_stats,
            research::search_sessions,
            fetch_sessions,
            create_session,
            update_session,
            delete_session,
            biblical_correction::suggest_biblical_terms,
            fetch_tts_diagnostics,
            open_windows_settings,
            speak_text,
            stop_speech,
            transcribe_audio
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_, _| {});
}
