use fs2::available_space;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use crate::database_migrations::{create_consistent_database_backup, open_database};
use crate::CURRENT_SCHEMA_VERSION;

const MANIFEST_VERSION: u32 = 1;
const BASELINE_CONTENT_VERSION: i64 = 1;
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_JSONL_LINE_BYTES: usize = 1024 * 1024;
const MAX_PACKAGE_ROWS: usize = 100_000;
const CONTENT_BACKUP_PREFIX: &str = "rhelo.backup-content";
const CONTENT_BACKUP_RETENTION: usize = 2;
const BACKUP_SAFETY_MARGIN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContentUpdateStatus {
    pub(crate) content_version: i64,
    pub(crate) applied_update_ids: Vec<String>,
    pub(crate) warning: Option<String>,
    pub(crate) baseline_duration_ms: u128,
    pub(crate) discovery_duration_ms: u128,
    pub(crate) validation_duration_ms: u128,
    pub(crate) backup_duration_ms: u128,
    pub(crate) application_duration_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BaselineManifest {
    manifest_version: u32,
    update_id: String,
    content_version: i64,
    source_version: String,
    payload_sha256: String,
    expected_schema_version: i32,
    expected_table_counts: BTreeMap<String, i64>,
    expected_translation_counts: BTreeMap<String, i64>,
    required_route_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum StaticTable {
    VerseTranslations,
    GeographyRoutes,
    RoutePoints,
    Commentaries,
    CrossReferences,
}

impl StaticTable {
    fn sql_name(self) -> &'static str {
        match self {
            Self::VerseTranslations => "verse_translations",
            Self::GeographyRoutes => "geography_routes",
            Self::RoutePoints => "route_points",
            Self::Commentaries => "commentaries",
            Self::CrossReferences => "cross_references",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OperationMode {
    InsertOnly,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadManifest {
    path: String,
    table: StaticTable,
    mode: OperationMode,
    sha256: String,
    expected_inserted_rows: usize,
    expected_updated_rows: usize,
    expected_deleted_rows: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentManifest {
    manifest_version: u32,
    update_id: String,
    content_version: i64,
    minimum_schema_version: i32,
    maximum_schema_version: Option<i32>,
    minimum_app_version: Option<String>,
    package_created_at: String,
    package_sha256: String,
    payload_files: Vec<PayloadManifest>,
    expected_inserted_rows: usize,
    expected_updated_rows: usize,
    expected_deleted_rows: usize,
    affected_static_tables: Vec<StaticTable>,
    fts_rebuild: Vec<String>,
    source_ids: Vec<String>,
    licence_ids: Vec<String>,
    attribution_references: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecksumsFile {
    manifest_version: u32,
    package_sha256: String,
    files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerseTranslationRow {
    verse_id: String,
    translation_code: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeographyRouteRow {
    route_id: String,
    title: String,
    description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutePointRow {
    route_id: String,
    sequence_order: i64,
    latitude: f64,
    longitude: f64,
    place_name: Option<String>,
    associated_verse_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentaryRow {
    commentary_id: String,
    verse_id: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrossReferenceRow {
    from_verse: String,
    to_verse: String,
    votes: Option<i64>,
}

#[derive(Clone, Debug)]
enum StaticRow {
    VerseTranslation(VerseTranslationRow),
    GeographyRoute(GeographyRouteRow),
    RoutePoint(RoutePointRow),
    Commentary(CommentaryRow),
    CrossReference(CrossReferenceRow),
}

#[derive(Clone, Debug)]
struct ValidatedPayload {
    table: StaticTable,
    rows: Vec<StaticRow>,
}

#[derive(Clone, Debug)]
struct ValidatedPackage {
    manifest: ContentManifest,
    payloads: Vec<ValidatedPayload>,
    total_bytes: u64,
}

#[derive(Clone, Debug)]
struct PackageCandidate {
    path: PathBuf,
    update_id: String,
    content_version: i64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_limited_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Content package file is unavailable: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Content package entries must be regular files, not links.".to_string());
    }
    if metadata.len() > maximum_bytes {
        return Err(format!(
            "Content package entry exceeds the {maximum_bytes}-byte limit."
        ));
    }
    fs::read(path).map_err(|error| format!("Content package file could not be read: {error}"))
}

fn validate_relative_payload_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path.components().count() != 1
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("Unsafe content package path: {value}"));
    }
    if !value.ends_with(".jsonl") {
        return Err(format!("Content payload must use JSONL: {value}"));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(format!("Invalid {label}."));
    }
    Ok(())
}

fn validate_translation_code(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("Invalid translation_code.".to_string());
    }
    Ok(())
}

fn validate_text(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty."));
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<Vec<u64>, String> {
    let core = value.split(['-', '+']).next().unwrap_or(value);
    let parts = core
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("Invalid application version: {value}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parts.is_empty() {
        return Err(format!("Invalid application version: {value}"));
    }
    Ok(parts)
}

fn is_version_at_least(current: &str, minimum: &str) -> Result<bool, String> {
    let mut current = parse_version(current)?;
    let mut minimum = parse_version(minimum)?;
    let width = current.len().max(minimum.len());
    current.resize(width, 0);
    minimum.resize(width, 0);
    Ok(current >= minimum)
}

fn parse_jsonl(table: StaticTable, bytes: &[u8]) -> Result<Vec<StaticRow>, String> {
    let content = std::str::from_utf8(bytes)
        .map_err(|_| "Content payload must be canonical UTF-8.".to_string())?;
    if content.contains('\r') {
        return Err("Content payload must use LF line endings.".to_string());
    }
    let mut rows = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.len() > MAX_JSONL_LINE_BYTES {
            return Err(format!("Content payload line {} is too large.", index + 1));
        }
        if line.trim().is_empty() {
            return Err(format!(
                "Content payload line {} must not be blank.",
                index + 1
            ));
        }
        let row = match table {
            StaticTable::VerseTranslations => StaticRow::VerseTranslation(
                serde_json::from_str(line)
                    .map_err(|error| format!("Malformed translation row {}: {error}", index + 1))?,
            ),
            StaticTable::GeographyRoutes => StaticRow::GeographyRoute(
                serde_json::from_str(line)
                    .map_err(|error| format!("Malformed route row {}: {error}", index + 1))?,
            ),
            StaticTable::RoutePoints => StaticRow::RoutePoint(
                serde_json::from_str(line)
                    .map_err(|error| format!("Malformed route-point row {}: {error}", index + 1))?,
            ),
            StaticTable::Commentaries => StaticRow::Commentary(
                serde_json::from_str(line)
                    .map_err(|error| format!("Malformed commentary row {}: {error}", index + 1))?,
            ),
            StaticTable::CrossReferences => {
                StaticRow::CrossReference(serde_json::from_str(line).map_err(|error| {
                    format!("Malformed cross-reference row {}: {error}", index + 1)
                })?)
            }
        };
        rows.push(row);
        if rows.len() > MAX_PACKAGE_ROWS {
            return Err("Content package contains too many rows.".to_string());
        }
    }
    Ok(rows)
}

fn aggregate_package_sha256(payloads: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (path, checksum) in payloads {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(checksum.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

fn validate_route_payloads(payloads: &[ValidatedPayload]) -> Result<(), String> {
    let route_ids = payloads
        .iter()
        .filter(|payload| payload.table == StaticTable::GeographyRoutes)
        .flat_map(|payload| payload.rows.iter())
        .filter_map(|row| match row {
            StaticRow::GeographyRoute(route) => Some(route.route_id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut points = HashMap::<&str, Vec<&RoutePointRow>>::new();
    for point in payloads
        .iter()
        .filter(|payload| payload.table == StaticTable::RoutePoints)
        .flat_map(|payload| payload.rows.iter())
        .filter_map(|row| match row {
            StaticRow::RoutePoint(point) => Some(point),
            _ => None,
        })
    {
        points
            .entry(point.route_id.as_str())
            .or_default()
            .push(point);
    }
    for route_id in &route_ids {
        let route_points = points.get(route_id).cloned().unwrap_or_default();
        if route_points.len() < 2 {
            return Err(format!(
                "Route {route_id} must include at least two ordered points."
            ));
        }
        let mut sequences = HashSet::new();
        for point in route_points {
            if !sequences.insert(point.sequence_order) {
                return Err(format!(
                    "Route {route_id} contains duplicate sequence_order values."
                ));
            }
        }
    }
    for route_id in points.keys() {
        if !route_ids.contains(route_id) {
            return Err(format!(
                "Route points for {route_id} require a route row in the same insert-only package."
            ));
        }
    }
    Ok(())
}

fn validate_package(package_path: &Path, schema_version: i32) -> Result<ValidatedPackage, String> {
    let package_metadata = fs::symlink_metadata(package_path)
        .map_err(|error| format!("Content package is unavailable: {error}"))?;
    if package_metadata.file_type().is_symlink() || !package_metadata.is_dir() {
        return Err("Content packages must be unpacked regular directories.".to_string());
    }

    let manifest_bytes = read_limited_file(&package_path.join("manifest.json"), MAX_PAYLOAD_BYTES)?;
    let manifest: ContentManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("Malformed content manifest: {error}"))?;
    let checksums_bytes =
        read_limited_file(&package_path.join("checksums.json"), MAX_PAYLOAD_BYTES)?;
    let checksums: ChecksumsFile = serde_json::from_slice(&checksums_bytes)
        .map_err(|error| format!("Malformed checksums file: {error}"))?;

    if manifest.manifest_version != MANIFEST_VERSION
        || checksums.manifest_version != MANIFEST_VERSION
    {
        return Err("Unsupported content manifest version.".to_string());
    }
    validate_identifier(&manifest.update_id, "update_id")?;
    if manifest.content_version <= BASELINE_CONTENT_VERSION {
        return Err("Content packages must be newer than the bundled baseline.".to_string());
    }
    if schema_version < manifest.minimum_schema_version
        || manifest
            .maximum_schema_version
            .is_some_and(|maximum| schema_version > maximum)
    {
        return Err("Content package is incompatible with the current schema version.".to_string());
    }
    if let Some(minimum_app_version) = &manifest.minimum_app_version {
        if !is_version_at_least(env!("CARGO_PKG_VERSION"), minimum_app_version)? {
            return Err("Content package requires a newer application version.".to_string());
        }
    }
    validate_text(&manifest.package_created_at, "package_created_at")?;
    if !is_sha256(&manifest.package_sha256)
        || !is_sha256(&checksums.package_sha256)
        || manifest.package_sha256 != checksums.package_sha256
    {
        return Err("Invalid aggregate package checksum.".to_string());
    }
    if manifest.expected_updated_rows != 0 || manifest.expected_deleted_rows != 0 {
        return Err("Schema v3 content packages support insert_only operations only.".to_string());
    }
    if !manifest.fts_rebuild.is_empty() {
        if manifest
            .fts_rebuild
            .iter()
            .any(|name| name == "sessions_fts")
        {
            return Err("Static content packages may never rebuild sessions_fts.".to_string());
        }
        return Err("Schema v3 does not permit static FTS rebuild operations.".to_string());
    }
    for id in manifest
        .source_ids
        .iter()
        .chain(&manifest.licence_ids)
        .chain(&manifest.attribution_references)
    {
        validate_identifier(id, "source/licence/attribution identifier")?;
    }

    let affected = manifest
        .affected_static_tables
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if affected.len() != manifest.affected_static_tables.len() {
        return Err("affected_static_tables contains duplicates.".to_string());
    }
    let payload_tables = manifest
        .payload_files
        .iter()
        .map(|payload| payload.table)
        .collect::<BTreeSet<_>>();
    if affected != payload_tables {
        return Err("affected_static_tables must exactly match payload tables.".to_string());
    }

    let mut expected_paths =
        BTreeSet::from(["manifest.json".to_string(), "checksums.json".to_string()]);
    let mut payload_hashes = BTreeMap::new();
    let mut payloads = Vec::new();
    let mut total_bytes = manifest_bytes.len() as u64 + checksums_bytes.len() as u64;
    let mut total_rows = 0usize;
    let mut declared_inserted = 0usize;
    for payload in &manifest.payload_files {
        if payload.mode != OperationMode::InsertOnly {
            return Err("Unsupported content operation mode.".to_string());
        }
        if payload.expected_updated_rows != 0 || payload.expected_deleted_rows != 0 {
            return Err("Insert-only payloads cannot declare updates or deletions.".to_string());
        }
        validate_relative_payload_path(&payload.path)?;
        if !expected_paths.insert(payload.path.clone()) {
            return Err(format!("Duplicate content package entry: {}", payload.path));
        }
        if !is_sha256(&payload.sha256) {
            return Err(format!("Invalid checksum for {}.", payload.path));
        }
        let checksum_entry = checksums
            .files
            .get(&payload.path)
            .ok_or_else(|| format!("Missing checksum entry for {}.", payload.path))?;
        if checksum_entry != &payload.sha256 {
            return Err(format!("Checksum metadata mismatch for {}.", payload.path));
        }
        let bytes = read_limited_file(&package_path.join(&payload.path), MAX_PAYLOAD_BYTES)?;
        total_bytes += bytes.len() as u64;
        if total_bytes > MAX_PACKAGE_BYTES {
            return Err("Content package exceeds the total size limit.".to_string());
        }
        let actual_sha256 = sha256_hex(&bytes);
        if actual_sha256 != payload.sha256 {
            return Err(format!("Checksum mismatch for {}.", payload.path));
        }
        let rows = parse_jsonl(payload.table, &bytes)?;
        if rows.len() != payload.expected_inserted_rows {
            return Err(format!(
                "Row-count mismatch for {}: expected {}, found {}.",
                payload.path,
                payload.expected_inserted_rows,
                rows.len()
            ));
        }
        total_rows += rows.len();
        declared_inserted += payload.expected_inserted_rows;
        if total_rows > MAX_PACKAGE_ROWS {
            return Err("Content package contains too many rows.".to_string());
        }
        payload_hashes.insert(payload.path.clone(), actual_sha256);
        payloads.push(ValidatedPayload {
            table: payload.table,
            rows,
        });
    }
    if checksums.files.keys().cloned().collect::<BTreeSet<_>>()
        != payload_hashes.keys().cloned().collect::<BTreeSet<_>>()
    {
        return Err("checksums.json contains unknown or missing payload entries.".to_string());
    }
    if declared_inserted != manifest.expected_inserted_rows || total_rows != declared_inserted {
        return Err("Manifest operation totals do not match payload rows.".to_string());
    }
    let aggregate = aggregate_package_sha256(&payload_hashes);
    if aggregate != manifest.package_sha256 {
        return Err("Aggregate content package checksum mismatch.".to_string());
    }

    let actual_entries = fs::read_dir(package_path)
        .map_err(|error| format!("Content package directory could not be read: {error}"))?
        .map(|entry| {
            entry
                .map_err(|error| format!("Content package entry could not be read: {error}"))
                .and_then(|entry| {
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "Content package filenames must be UTF-8.".to_string())
                })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual_entries != expected_paths {
        return Err("Content package contains undeclared or missing entries.".to_string());
    }
    validate_route_payloads(&payloads)?;

    Ok(ValidatedPackage {
        manifest,
        payloads,
        total_bytes,
    })
}

fn row_exists(
    connection: &Connection,
    sql: &str,
    values: &[&dyn rusqlite::ToSql],
) -> Result<bool, String> {
    connection
        .query_row(sql, values, |_| Ok(()))
        .optional()
        .map(|value| value.is_some())
        .map_err(|error| format!("Content validation query failed: {error}"))
}

fn require_verse(connection: &Connection, verse_id: &str) -> Result<(), String> {
    validate_identifier(verse_id, "canonical verse_id")?;
    if !row_exists(
        connection,
        "SELECT 1 FROM verses_base WHERE id = ?1",
        &[&verse_id],
    )? {
        return Err(format!("Unknown canonical verse_id: {verse_id}"));
    }
    Ok(())
}

fn apply_static_row(connection: &Connection, row: &StaticRow) -> Result<(), String> {
    match row {
        StaticRow::VerseTranslation(row) => {
            require_verse(connection, &row.verse_id)?;
            validate_translation_code(&row.translation_code)?;
            validate_text(&row.text, "Translation text")?;
            connection
                .execute(
                    "INSERT INTO verse_translations (verse_id, translation_code, text)
                     VALUES (?1, ?2, ?3)",
                    params![row.verse_id, row.translation_code, row.text],
                )
                .map_err(|error| format!("Translation insert failed: {error}"))?;
        }
        StaticRow::GeographyRoute(row) => {
            validate_identifier(&row.route_id, "route_id")?;
            validate_text(&row.title, "Route title")?;
            connection
                .execute(
                    "INSERT INTO geography_routes (route_id, title, description)
                     VALUES (?1, ?2, ?3)",
                    params![row.route_id, row.title, row.description],
                )
                .map_err(|error| format!("Route insert failed: {error}"))?;
        }
        StaticRow::RoutePoint(row) => {
            validate_identifier(&row.route_id, "route_id")?;
            if row.sequence_order < 0 {
                return Err("Route sequence_order must be non-negative.".to_string());
            }
            if !row.latitude.is_finite() || !(-90.0..=90.0).contains(&row.latitude) {
                return Err("Route latitude must be between -90 and 90.".to_string());
            }
            if !row.longitude.is_finite() || !(-180.0..=180.0).contains(&row.longitude) {
                return Err("Route longitude must be between -180 and 180.".to_string());
            }
            if let Some(verse_id) = &row.associated_verse_id {
                require_verse(connection, verse_id)?;
            }
            connection
                .execute(
                    "INSERT INTO route_points (
                        route_id, sequence_order, latitude, longitude,
                        place_name, associated_verse_id
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        row.route_id,
                        row.sequence_order,
                        row.latitude,
                        row.longitude,
                        row.place_name,
                        row.associated_verse_id
                    ],
                )
                .map_err(|error| format!("Route-point insert failed: {error}"))?;
        }
        StaticRow::Commentary(row) => {
            validate_identifier(&row.commentary_id, "commentary_id")?;
            require_verse(connection, &row.verse_id)?;
            validate_text(&row.text, "Commentary text")?;
            connection
                .execute(
                    "INSERT INTO commentaries (commentary_id, verse_id, text)
                     VALUES (?1, ?2, ?3)",
                    params![row.commentary_id, row.verse_id, row.text],
                )
                .map_err(|error| format!("Commentary insert failed: {error}"))?;
        }
        StaticRow::CrossReference(row) => {
            require_verse(connection, &row.from_verse)?;
            require_verse(connection, &row.to_verse)?;
            if row_exists(
                connection,
                "SELECT 1 FROM cross_references
                 WHERE from_verse = ?1 AND to_verse = ?2",
                &[&row.from_verse, &row.to_verse],
            )? {
                return Err("Duplicate cross-reference row.".to_string());
            }
            connection
                .execute(
                    "INSERT INTO cross_references (from_verse, to_verse, votes)
                     VALUES (?1, ?2, ?3)",
                    params![row.from_verse, row.to_verse, row.votes],
                )
                .map_err(|error| format!("Cross-reference insert failed: {error}"))?;
        }
    }
    Ok(())
}

fn verify_targeted_orphans(
    connection: &Connection,
    tables: &BTreeSet<StaticTable>,
) -> Result<(), String> {
    let checks = [
        (
            StaticTable::VerseTranslations,
            "SELECT COUNT(*) FROM verse_translations child
             WHERE NOT EXISTS (SELECT 1 FROM verses_base v WHERE v.id = child.verse_id)",
        ),
        (
            StaticTable::RoutePoints,
            "SELECT COUNT(*) FROM route_points child
             WHERE child.associated_verse_id IS NOT NULL
               AND NOT EXISTS (
                 SELECT 1 FROM verses_base v WHERE v.id = child.associated_verse_id
               )",
        ),
        (
            StaticTable::Commentaries,
            "SELECT COUNT(*) FROM commentaries child
             WHERE child.verse_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM verses_base v WHERE v.id = child.verse_id)",
        ),
        (
            StaticTable::CrossReferences,
            "SELECT COUNT(*) FROM cross_references child
             WHERE NOT EXISTS (SELECT 1 FROM verses_base v WHERE v.id = child.from_verse)
                OR NOT EXISTS (SELECT 1 FROM verses_base v WHERE v.id = child.to_verse)",
        ),
    ];
    for (table, sql) in checks {
        if !tables.contains(&table) {
            continue;
        }
        let count: i64 = connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(|error| format!("Content orphan validation failed: {error}"))?;
        if count != 0 {
            return Err(format!(
                "Content update left {count} orphan rows in {}.",
                table.sql_name()
            ));
        }
    }
    Ok(())
}

fn current_content_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(content_version), 0) FROM content_updates",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Content version could not be read: {error}"))
}

fn is_update_applied(
    connection: &Connection,
    update_id: &str,
    content_version: i64,
) -> Result<bool, String> {
    let existing = connection
        .query_row(
            "SELECT update_id, content_version FROM content_updates
             WHERE update_id = ?1 OR content_version = ?2",
            params![update_id, content_version],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| format!("Content update history could not be read: {error}"))?;
    match existing {
        None => Ok(false),
        Some((existing_id, existing_version))
            if existing_id == update_id && existing_version == content_version =>
        {
            Ok(true)
        }
        Some(_) => Err("Duplicate update_id or content_version conflict.".to_string()),
    }
}

fn apply_package(database_path: &Path, package: &ValidatedPackage) -> Result<u128, String> {
    let started = Instant::now();
    let mut connection = open_database(database_path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("Content update transaction could not start: {error}"))?;
    let schema_version: i32 = transaction
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("Content update schema could not be revalidated: {error}"))?;
    if schema_version < package.manifest.minimum_schema_version
        || package
            .manifest
            .maximum_schema_version
            .is_some_and(|maximum| schema_version > maximum)
    {
        return Err("Content package schema compatibility changed before apply.".to_string());
    }
    if is_update_applied(
        &transaction,
        &package.manifest.update_id,
        package.manifest.content_version,
    )? {
        return Ok(0);
    }
    let current_version = current_content_version(&transaction)?;
    if package.manifest.content_version <= current_version {
        return Err("Content-version rollback or out-of-order update rejected.".to_string());
    }

    let mut inserted_rows = 0usize;
    let mut affected_tables = BTreeSet::new();
    for payload in &package.payloads {
        affected_tables.insert(payload.table);
        for row in &payload.rows {
            apply_static_row(&transaction, row)?;
            inserted_rows += 1;
        }
    }
    if inserted_rows != package.manifest.expected_inserted_rows {
        return Err("Applied content row count did not match the manifest.".to_string());
    }
    verify_targeted_orphans(&transaction, &affected_tables)?;
    let duration_ms = started.elapsed().as_millis();
    transaction
        .execute(
            "INSERT INTO content_updates (
                update_id, content_version, payload_sha256, applied_at,
                app_version, manifest_version, source_version, row_count, duration_ms
             ) VALUES (
                ?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                ?4, ?5, ?6, ?7, ?8
             )",
            params![
                package.manifest.update_id,
                package.manifest.content_version,
                package.manifest.package_sha256,
                env!("CARGO_PKG_VERSION"),
                package.manifest.manifest_version,
                package.manifest.source_ids.join(","),
                inserted_rows as i64,
                duration_ms as i64
            ],
        )
        .map_err(|error| format!("Content update record could not be written: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Content update transaction could not commit: {error}"))?;
    Ok(duration_ms)
}

fn baseline_identity(manifest: &BaselineManifest) -> String {
    let mut hasher = Sha256::new();
    for (name, count) in &manifest.expected_table_counts {
        hasher.update(format!("table:{name}={count}\n"));
    }
    for (code, count) in &manifest.expected_translation_counts {
        hasher.update(format!("translation:{code}={count}\n"));
    }
    let mut routes = manifest.required_route_ids.clone();
    routes.sort();
    for route_id in routes {
        hasher.update(format!("route:{route_id}\n"));
    }
    format!("{:x}", hasher.finalize())
}

fn establish_baseline(connection: &Connection, baseline_path: &Path) -> Result<i64, String> {
    let existing_version = current_content_version(connection)?;
    if existing_version > 0 {
        return Ok(existing_version);
    }
    let bytes = read_limited_file(baseline_path, MAX_PAYLOAD_BYTES)?;
    let manifest: BaselineManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Bundled baseline manifest is malformed: {error}"))?;
    if manifest.manifest_version != MANIFEST_VERSION
        || manifest.content_version != BASELINE_CONTENT_VERSION
        || manifest.expected_schema_version != CURRENT_SCHEMA_VERSION
    {
        return Err("Bundled baseline manifest is incompatible.".to_string());
    }
    validate_identifier(&manifest.update_id, "baseline update_id")?;
    if !is_sha256(&manifest.payload_sha256)
        || baseline_identity(&manifest) != manifest.payload_sha256
    {
        return Err("Bundled baseline fingerprint is invalid.".to_string());
    }

    const BASELINE_TABLES: &[&str] = &[
        "commentaries",
        "cross_references",
        "geography_routes",
        "route_points",
        "verse_translations",
        "verses_base",
    ];
    let baseline_table_set = BASELINE_TABLES
        .iter()
        .map(|table| (*table).to_string())
        .collect::<BTreeSet<_>>();
    if manifest
        .expected_table_counts
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != baseline_table_set
    {
        return Err("Bundled baseline must declare every required static table.".to_string());
    }
    for (table, expected) in &manifest.expected_table_counts {
        if !BASELINE_TABLES.contains(&table.as_str()) {
            return Err(format!("Unknown baseline table: {table}"));
        }
        let actual: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| format!("Baseline table {table} could not be checked: {error}"))?;
        if actual != *expected {
            return Err(format!(
                "Database does not match the recognized baseline: {table} expected {expected}, found {actual}"
            ));
        }
    }
    let translation_counts = connection
        .prepare(
            "SELECT translation_code, COUNT(*)
             FROM verse_translations GROUP BY translation_code ORDER BY translation_code",
        )
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<BTreeMap<_, _>, _>>()
        })
        .map_err(|error| format!("Baseline translations could not be checked: {error}"))?;
    if translation_counts != manifest.expected_translation_counts {
        return Err(
            "Database translation catalogue does not match the recognized baseline.".to_string(),
        );
    }
    for route_id in &manifest.required_route_ids {
        if !row_exists(
            connection,
            "SELECT 1 FROM geography_routes WHERE route_id = ?1",
            &[route_id],
        )? {
            return Err(format!("Recognized baseline route is missing: {route_id}"));
        }
    }
    let row_count = manifest.expected_table_counts.values().sum::<i64>();
    connection
        .execute(
            "INSERT INTO content_updates (
                update_id, content_version, payload_sha256, applied_at,
                app_version, manifest_version, source_version, row_count, duration_ms
             ) VALUES (
                ?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                ?4, ?5, ?6, ?7, 0
             )",
            params![
                manifest.update_id,
                manifest.content_version,
                manifest.payload_sha256,
                env!("CARGO_PKG_VERSION"),
                manifest.manifest_version,
                manifest.source_version,
                row_count
            ],
        )
        .map_err(|error| format!("Bundled content baseline could not be recorded: {error}"))?;
    Ok(manifest.content_version)
}

fn read_package_candidate(path: &Path) -> Result<PackageCandidate, String> {
    let bytes = read_limited_file(&path.join("manifest.json"), MAX_PAYLOAD_BYTES)?;
    let manifest: ContentManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Malformed content manifest: {error}"))?;
    validate_identifier(&manifest.update_id, "update_id")?;
    Ok(PackageCandidate {
        path: path.to_path_buf(),
        update_id: manifest.update_id,
        content_version: manifest.content_version,
    })
}

fn discover_packages(root: &Path) -> Result<Vec<PackageCandidate>, String> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|error| format!("Bundled content directory could not be read: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("Bundled content entry could not be read: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Bundled content entry names must be UTF-8.".to_string())?;
        if name == "baseline.json" || name.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Bundled content entry type could not be read: {error}"))?;
        if !file_type.is_dir() || !name.starts_with("content-update-") {
            return Err(format!("Unexpected bundled content entry: {name}"));
        }
        packages.push(read_package_candidate(&entry.path())?);
    }
    packages.sort_by_key(|package| package.content_version);
    let mut versions = HashSet::new();
    let mut ids = HashSet::new();
    for package in &packages {
        if !versions.insert(package.content_version) || !ids.insert(package.update_id.clone()) {
            return Err("Bundled content packages contain duplicate IDs or versions.".to_string());
        }
    }
    Ok(packages)
}

fn ensure_backup_space(
    database_path: &Path,
    pending_bytes: u64,
    available: u64,
) -> Result<(), String> {
    let database_bytes = fs::metadata(database_path)
        .map_err(|error| format!("Database size could not be read: {error}"))?
        .len();
    let required = database_bytes
        .saturating_add(pending_bytes)
        .saturating_add(BACKUP_SAFETY_MARGIN_BYTES);
    if available < required {
        return Err(format!(
            "Insufficient disk space for a safe content update: {required} bytes required."
        ));
    }
    Ok(())
}

fn content_backup_path(
    database_path: &Path,
    from_version: i64,
    to_version: i64,
) -> Result<PathBuf, String> {
    let parent = database_path
        .parent()
        .ok_or_else(|| "Database path has no application-data directory.".to_string())?;
    for suffix in 0.. {
        let suffix_text = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = parent.join(format!(
            "{CONTENT_BACKUP_PREFIX}-v{from_version}-to-v{to_version}{suffix_text}.sqlite3"
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Content backup path could not be reserved: {error}"
                ))
            }
        }
    }
    unreachable!("the collision-safe content backup suffix space is unbounded")
}

fn retain_recent_content_backups(database_path: &Path) -> Result<(), String> {
    let parent = database_path
        .parent()
        .ok_or_else(|| "Database path has no application-data directory.".to_string())?;
    let mut backups = fs::read_dir(parent)
        .map_err(|error| format!("Content backup directory could not be read: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(CONTENT_BACKUP_PREFIX)
        })
        .collect::<Vec<_>>();
    backups.sort_by_key(|entry| {
        std::cmp::Reverse(
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        )
    });
    for old in backups.into_iter().skip(CONTENT_BACKUP_RETENTION) {
        fs::remove_file(old.path())
            .map_err(|error| format!("Old content backup could not be removed: {error}"))?;
    }
    Ok(())
}

pub(crate) fn process_bundled_content_updates(
    database_path: &Path,
    content_root: &Path,
) -> Result<ContentUpdateStatus, String> {
    let mut status = ContentUpdateStatus::default();
    let baseline_started = Instant::now();
    let connection = open_database(database_path)?;
    status.content_version = establish_baseline(&connection, &content_root.join("baseline.json"))?;
    status.baseline_duration_ms = baseline_started.elapsed().as_millis();

    let discovery_started = Instant::now();
    let candidates = discover_packages(content_root)?;
    status.discovery_duration_ms = discovery_started.elapsed().as_millis();
    let mut pending = Vec::new();
    let validation_started = Instant::now();
    for candidate in candidates {
        if is_update_applied(&connection, &candidate.update_id, candidate.content_version)? {
            continue;
        }
        if candidate.content_version <= status.content_version {
            return Err("Out-of-order bundled content version rejected.".to_string());
        }
        pending.push(validate_package(&candidate.path, CURRENT_SCHEMA_VERSION)?);
    }
    status.validation_duration_ms = validation_started.elapsed().as_millis();
    drop(connection);
    if pending.is_empty() {
        return Ok(status);
    }

    let pending_bytes = pending.iter().map(|package| package.total_bytes).sum();
    let parent = database_path
        .parent()
        .ok_or_else(|| "Database path has no application-data directory.".to_string())?;
    ensure_backup_space(
        database_path,
        pending_bytes,
        available_space(parent)
            .map_err(|error| format!("Available disk space could not be checked: {error}"))?,
    )?;
    let backup_started = Instant::now();
    let target_version = pending
        .last()
        .map(|package| package.manifest.content_version)
        .unwrap_or(status.content_version);
    let backup_path = content_backup_path(database_path, status.content_version, target_version)?;
    create_consistent_database_backup(database_path, &backup_path)?;
    retain_recent_content_backups(database_path)?;
    status.backup_duration_ms = backup_started.elapsed().as_millis();

    let application_started = Instant::now();
    for package in pending {
        apply_package(database_path, &package)?;
        status.content_version = package.manifest.content_version;
        status
            .applied_update_ids
            .push(package.manifest.update_id.clone());
    }
    status.application_duration_ms = application_started.elapsed().as_millis();
    Ok(status)
}

pub(crate) fn process_bundled_content_updates_safely(
    database_path: &Path,
    content_root: &Path,
) -> ContentUpdateStatus {
    match process_bundled_content_updates(database_path, content_root) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("CONTENT UPDATE DIAGNOSTIC: {error}");
            let content_version = open_database(database_path)
                .and_then(|connection| current_content_version(&connection))
                .unwrap_or(0);
            ContentUpdateStatus {
                content_version,
                warning: Some(
                    "An optional bundled content update could not be installed. Rhelo kept the existing content and will retry on the next launch."
                        .to_string(),
                ),
                ..ContentUpdateStatus::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("rhelo-content-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fixture_package() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/content-update-0002")
    }

    fn copy_package(target_parent: &Path) -> PathBuf {
        let target = target_parent.join("content-update-0002");
        fs::create_dir_all(&target).unwrap();
        for entry in fs::read_dir(fixture_package()).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
        target
    }

    fn create_fixture_database(path: &Path, include_baseline: bool) {
        let connection = open_database(path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE verses_base (
                    id TEXT PRIMARY KEY,
                    book TEXT,
                    chapter INTEGER,
                    verse INTEGER
                );
                CREATE TABLE verse_translations (
                    verse_id TEXT,
                    translation_code TEXT,
                    text TEXT,
                    PRIMARY KEY (verse_id, translation_code)
                );
                CREATE TABLE geography_routes (
                    route_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    description TEXT
                );
                CREATE TABLE route_points (
                    route_id TEXT,
                    sequence_order INTEGER,
                    latitude REAL NOT NULL,
                    longitude REAL NOT NULL,
                    place_name TEXT,
                    associated_verse_id TEXT,
                    PRIMARY KEY (route_id, sequence_order),
                    FOREIGN KEY (route_id) REFERENCES geography_routes(route_id)
                );
                CREATE TABLE commentaries (
                    commentary_id TEXT,
                    verse_id TEXT,
                    text TEXT,
                    PRIMARY KEY(commentary_id, verse_id),
                    FOREIGN KEY(verse_id) REFERENCES verses_base(id)
                );
                CREATE TABLE cross_references (
                    from_verse TEXT,
                    to_verse TEXT,
                    votes INTEGER
                );
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
                CREATE TABLE sessions (
                    session_id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    content TEXT
                );
                CREATE TABLE session_documents (
                    document_id TEXT PRIMARY KEY,
                    session_id TEXT,
                    file_path TEXT NOT NULL
                );
                CREATE VIRTUAL TABLE sessions_fts USING fts5(
                    session_id UNINDEXED, title, content
                );
                CREATE TABLE chat_history (
                    message_id TEXT PRIMARY KEY,
                    session_id TEXT,
                    role TEXT,
                    content TEXT
                );
                CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                INSERT INTO verses_base VALUES
                    ('GEN.1.1', 'GEN', 1, 1),
                    ('GEN.1.2', 'GEN', 1, 2);
                INSERT INTO sessions VALUES (
                    'session-1', 'Keep',
                    '<p><strong>User HTML remains byte-for-byte unchanged.</strong></p>'
                );
                INSERT INTO session_documents VALUES (
                    'document-1', 'session-1', '/private/reference.pdf'
                );
                INSERT INTO sessions_fts VALUES (
                    'session-1', 'Keep',
                    '<p><strong>User HTML remains byte-for-byte unchanged.</strong></p>'
                );
                INSERT INTO chat_history VALUES (
                    'message-1', 'session-1', 'user', 'Private chat remains unchanged.'
                );
                INSERT INTO settings VALUES ('translation_order', '[\"one\",\"two\"]');
                PRAGMA user_version = 3;
                ",
            )
            .unwrap();
        if include_baseline {
            connection
                .execute(
                    "INSERT INTO content_updates VALUES (
                        'test-baseline', 1, ?1, '2026-07-28T00:00:00Z',
                        '0.1.1', 1, 'fixture', 2, 0
                     )",
                    [format!("{:064}", 0)],
                )
                .unwrap();
        }
    }

    fn rewrite_payload(package: &Path, path: &str, bytes: &[u8]) {
        fs::write(package.join(path), bytes).unwrap();
        let checksum = sha256_hex(bytes);
        let manifest_path = package.join("manifest.json");
        let checksums_path = package.join("checksums.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let payloads = manifest["payload_files"].as_array_mut().unwrap();
        for payload in payloads.iter_mut() {
            if payload["path"] == path {
                payload["sha256"] = Value::String(checksum.clone());
                payload["expected_inserted_rows"] =
                    Value::from(std::str::from_utf8(bytes).unwrap().lines().count());
            }
        }
        manifest["expected_inserted_rows"] = Value::from(
            payloads
                .iter()
                .map(|payload| payload["expected_inserted_rows"].as_u64().unwrap())
                .sum::<u64>(),
        );

        let mut checksums: Value =
            serde_json::from_slice(&fs::read(&checksums_path).unwrap()).unwrap();
        checksums["files"][path] = Value::String(checksum);
        let hashes = checksums["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, value)| (path.clone(), value.as_str().unwrap().to_string()))
            .collect::<BTreeMap<_, _>>();
        let aggregate = aggregate_package_sha256(&hashes);
        manifest["package_sha256"] = Value::String(aggregate.clone());
        checksums["package_sha256"] = Value::String(aggregate);
        fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        fs::write(
            checksums_path,
            serde_json::to_vec_pretty(&checksums).unwrap(),
        )
        .unwrap();
    }

    fn write_fixture_baseline(root: &Path) {
        let mut baseline = BaselineManifest {
            manifest_version: 1,
            update_id: "test-baseline-v1".to_string(),
            content_version: 1,
            source_version: "test-fixture".to_string(),
            payload_sha256: String::new(),
            expected_schema_version: 3,
            expected_table_counts: BTreeMap::from([
                ("commentaries".to_string(), 0),
                ("cross_references".to_string(), 0),
                ("geography_routes".to_string(), 0),
                ("route_points".to_string(), 0),
                ("verse_translations".to_string(), 0),
                ("verses_base".to_string(), 2),
            ]),
            expected_translation_counts: BTreeMap::new(),
            required_route_ids: Vec::new(),
        };
        baseline.payload_sha256 = baseline_identity(&baseline);
        fs::write(
            root.join("baseline.json"),
            serde_json::to_vec_pretty(&baseline).unwrap(),
        )
        .unwrap();
    }

    fn package_row_count(connection: &Connection) -> i64 {
        connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM verse_translations WHERE translation_code = 'test_phase3')
                  + (SELECT COUNT(*) FROM geography_routes WHERE route_id = 'test_phase3_route')
                  + (SELECT COUNT(*) FROM route_points WHERE route_id = 'test_phase3_route')
                  + (SELECT COUNT(*) FROM commentaries WHERE commentary_id = 'test_phase3_study')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn user_data_fingerprint(connection: &Connection) -> String {
        let values = [
            connection
                .query_row(
                    "SELECT session_id || title || content FROM sessions",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT document_id || session_id || file_path FROM session_documents",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT session_id || title || content FROM sessions_fts",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row(
                    "SELECT message_id || session_id || role || content FROM chat_history",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            connection
                .query_row("SELECT key || value FROM settings", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
        ];
        sha256_hex(values.join("\n").as_bytes())
    }

    #[test]
    fn fixture_package_validates_applies_once_and_preserves_ordered_route() {
        let root = temp_dir("apply");
        let database_path = root.join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        let before_user_data = user_data_fingerprint(&open_database(&database_path).unwrap());
        let validated = validate_package(&fixture_package(), 3).unwrap();
        assert_eq!(validated.manifest.expected_inserted_rows, 5);

        apply_package(&database_path, &validated).unwrap();
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 5);
        assert_eq!(user_data_fingerprint(&connection), before_user_data);
        assert_eq!(current_content_version(&connection).unwrap(), 2);
        let sequences = connection
            .prepare(
                "SELECT sequence_order FROM route_points
                 WHERE route_id = 'test_phase3_route' ORDER BY sequence_order",
            )
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap();
        assert_eq!(sequences, vec![0, 1]);
        drop(connection);

        apply_package(&database_path, &validated).unwrap();
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 5);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM content_updates WHERE content_version = 2",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn checksum_manifest_table_path_and_size_guards_reject_before_mutation() {
        let root = temp_dir("guards");
        let package = copy_package(&root);
        fs::write(package.join("translations.jsonl"), b"tampered").unwrap();
        assert!(validate_package(&package, 3)
            .unwrap_err()
            .contains("Checksum mismatch"));

        let package = copy_package(&temp_dir("malformed-manifest"));
        fs::write(package.join("manifest.json"), b"{").unwrap();
        assert!(validate_package(&package, 3)
            .unwrap_err()
            .contains("Malformed content manifest"));

        let package = copy_package(&temp_dir("user-table"));
        let manifest_path = package.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["payload_files"][0]["table"] = Value::String("sessions".to_string());
        manifest["affected_static_tables"] = json!([
            "sessions",
            "geography_routes",
            "route_points",
            "commentaries"
        ]);
        fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        assert!(validate_package(&package, 3).is_err());

        let package = copy_package(&temp_dir("path-traversal"));
        let manifest_path = package.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["payload_files"][0]["path"] = Value::String("../translations.jsonl".to_string());
        fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        assert!(validate_package(&package, 3)
            .unwrap_err()
            .contains("Unsafe content package path"));

        let oversized = temp_dir("oversized").join("large.jsonl");
        fs::write(&oversized, vec![b'x'; MAX_PAYLOAD_BYTES as usize + 1]).unwrap();
        assert!(read_limited_file(&oversized, MAX_PAYLOAD_BYTES)
            .unwrap_err()
            .contains("exceeds"));
        assert!(parse_jsonl(StaticTable::Commentaries, b"{not-json}\n").is_err());
    }

    #[test]
    fn schema_coordinates_duplicates_and_out_of_order_versions_are_rejected() {
        assert!(validate_package(&fixture_package(), 2)
            .unwrap_err()
            .contains("incompatible"));

        let package = copy_package(&temp_dir("coordinate"));
        rewrite_payload(
            &package,
            "route-points.jsonl",
            b"{\"route_id\":\"test_phase3_route\",\"sequence_order\":0,\"latitude\":91,\"longitude\":35,\"place_name\":\"A\",\"associated_verse_id\":\"GEN.1.1\"}\n{\"route_id\":\"test_phase3_route\",\"sequence_order\":1,\"latitude\":31,\"longitude\":35,\"place_name\":\"B\",\"associated_verse_id\":\"GEN.1.2\"}\n",
        );
        let database_path = temp_dir("coordinate-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        let validated = validate_package(&package, 3).unwrap();
        assert!(apply_package(&database_path, &validated)
            .unwrap_err()
            .contains("latitude"));
        assert_eq!(
            package_row_count(&open_database(&database_path).unwrap()),
            0
        );

        let database_path = temp_dir("duplicate-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        let connection = open_database(&database_path).unwrap();
        connection
            .execute(
                "INSERT INTO verse_translations VALUES ('GEN.1.1', 'test_phase3', 'Existing')",
                [],
            )
            .unwrap();
        drop(connection);
        let validated = validate_package(&fixture_package(), 3).unwrap();
        assert!(apply_package(&database_path, &validated)
            .unwrap_err()
            .contains("Translation insert failed"));

        let database_path = temp_dir("order-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        let connection = open_database(&database_path).unwrap();
        connection
            .execute(
                "INSERT INTO content_updates VALUES (
                    'newer', 3, ?1, '2026-07-28T00:00:00Z',
                    '0.1.1', 1, 'fixture', 0, 0
                 )",
                [format!("{:064}", 3)],
            )
            .unwrap();
        drop(connection);
        assert!(apply_package(&database_path, &validated)
            .unwrap_err()
            .contains("out-of-order"));
    }

    #[test]
    fn mid_import_failure_rolls_back_every_row_and_retry_succeeds() {
        let package_root = temp_dir("rollback-package");
        let package = copy_package(&package_root);
        rewrite_payload(
            &package,
            "studies.jsonl",
            b"{\"commentary_id\":\"test_phase3_study\",\"verse_id\":\"BAD.1.1\",\"text\":\"Must fail.\"}\n",
        );
        let database_path = temp_dir("rollback-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        let invalid = validate_package(&package, 3).unwrap();
        assert!(apply_package(&database_path, &invalid)
            .unwrap_err()
            .contains("Unknown canonical verse_id"));
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 0);
        assert_eq!(current_content_version(&connection).unwrap(), 1);
        drop(connection);

        let valid = validate_package(&fixture_package(), 3).unwrap();
        apply_package(&database_path, &valid).unwrap();
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 5);
        assert_eq!(current_content_version(&connection).unwrap(), 2);
    }

    #[test]
    fn startup_records_baseline_applies_new_package_and_skips_historical_payload_hashing() {
        let root = temp_dir("startup");
        write_fixture_baseline(&root);
        let package = copy_package(&root);
        let database_path = temp_dir("startup-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, false);

        let started = Instant::now();
        let first = process_bundled_content_updates(&database_path, &root).unwrap();
        let first_elapsed = started.elapsed();
        assert_eq!(first.content_version, 2);
        assert_eq!(first.applied_update_ids, vec!["test-content-update-0002"]);
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 5);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM content_updates", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT update_id FROM content_updates WHERE content_version = 1",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "test-baseline-v1"
        );
        drop(connection);

        fs::write(package.join("translations.jsonl"), b"corrupt after apply").unwrap();
        let repeated_started = Instant::now();
        let second = process_bundled_content_updates(&database_path, &root).unwrap();
        let repeated_elapsed = repeated_started.elapsed();
        assert_eq!(second.content_version, 2);
        assert!(second.applied_update_ids.is_empty());
        assert_eq!(
            package_row_count(&open_database(&database_path).unwrap()),
            5
        );
        eprintln!(
            "content startup first={}ms repeated={}ms validation={}ms apply={}ms backup={}ms",
            first_elapsed.as_millis(),
            repeated_elapsed.as_millis(),
            first.validation_duration_ms,
            first.application_duration_ms,
            first.backup_duration_ms
        );
        assert!(first_elapsed < Duration::from_secs(5));
        assert!(repeated_elapsed < Duration::from_secs(1));
    }

    #[test]
    fn insufficient_space_is_rejected_before_backup() {
        let root = temp_dir("space");
        let database_path = root.join("rhelo.sqlite3");
        create_fixture_database(&database_path, true);
        assert!(ensure_backup_space(&database_path, 1024, 1)
            .unwrap_err()
            .contains("Insufficient disk space"));
    }

    #[test]
    fn duplicate_update_ids_and_versions_are_rejected() {
        let validated = validate_package(&fixture_package(), 3).unwrap();

        let duplicate_id_path = temp_dir("duplicate-id").join("rhelo.sqlite3");
        create_fixture_database(&duplicate_id_path, true);
        let connection = open_database(&duplicate_id_path).unwrap();
        connection
            .execute(
                "INSERT INTO content_updates VALUES (
                    'test-content-update-0002', 9, ?1, '2026-07-28T00:00:00Z',
                    '0.1.1', 1, 'fixture', 0, 0
                 )",
                [format!("{:064}", 9)],
            )
            .unwrap();
        drop(connection);
        assert!(apply_package(&duplicate_id_path, &validated)
            .unwrap_err()
            .contains("conflict"));

        let duplicate_version_path = temp_dir("duplicate-version").join("rhelo.sqlite3");
        create_fixture_database(&duplicate_version_path, true);
        let connection = open_database(&duplicate_version_path).unwrap();
        connection
            .execute(
                "INSERT INTO content_updates VALUES (
                    'different-update', 2, ?1, '2026-07-28T00:00:00Z',
                    '0.1.1', 1, 'fixture', 0, 0
                 )",
                [format!("{:064}", 2)],
            )
            .unwrap();
        drop(connection);
        assert!(apply_package(&duplicate_version_path, &validated)
            .unwrap_err()
            .contains("conflict"));
    }

    #[test]
    fn optional_package_failure_returns_warning_and_preserves_existing_content() {
        let root = temp_dir("safe-failure");
        write_fixture_baseline(&root);
        let package = copy_package(&root);
        fs::write(package.join("translations.jsonl"), b"corrupt").unwrap();
        let database_path = temp_dir("safe-failure-db").join("rhelo.sqlite3");
        create_fixture_database(&database_path, false);
        let before_user_data = user_data_fingerprint(&open_database(&database_path).unwrap());

        let status = process_bundled_content_updates_safely(&database_path, &root);
        assert_eq!(status.content_version, 1);
        assert!(status.warning.as_deref().unwrap().contains("will retry"));
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 0);
        assert_eq!(user_data_fingerprint(&connection), before_user_data);
    }

    #[test]
    fn migrated_bundled_copy_accepts_only_the_synthetic_test_package() {
        let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rhelo.db");
        if !bundled.exists() {
            return;
        }
        let database_path = temp_dir("bundled-package-db").join("rhelo.sqlite3");
        fs::copy(&bundled, &database_path).unwrap();
        crate::database_migrations::ensure_database_schema(&database_path, false).unwrap();

        let root = temp_dir("bundled-package-root");
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources/content-updates/baseline.json"),
            root.join("baseline.json"),
        )
        .unwrap();
        copy_package(&root);
        let before_bytes = fs::metadata(&database_path).unwrap().len();
        let first = process_bundled_content_updates(&database_path, &root).unwrap();
        assert_eq!(first.applied_update_ids, vec!["test-content-update-0002"]);
        let second = process_bundled_content_updates(&database_path, &root).unwrap();
        assert!(second.applied_update_ids.is_empty());
        let connection = open_database(&database_path).unwrap();
        assert_eq!(package_row_count(&connection), 5);
        assert_eq!(
            connection
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        let mut foreign_check = connection.prepare("PRAGMA foreign_key_check").unwrap();
        assert!(foreign_check.query([]).unwrap().next().unwrap().is_none());
        let after_bytes = fs::metadata(&database_path).unwrap().len();
        eprintln!(
            "full package validation={}ms backup={}ms apply={}ms size_delta={}",
            first.validation_duration_ms,
            first.backup_duration_ms,
            first.application_duration_ms,
            after_bytes as i128 - before_bytes as i128
        );
    }
}
