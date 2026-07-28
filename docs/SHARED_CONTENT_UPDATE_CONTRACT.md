# Shared Content Update Contract

## Scope

This contract defines non-executable, app-release-bundled static-content
packages for Rhelo Desktop, iPad, and future Android implementations. It does
not define platform-specific filesystem paths, UI code, network downloads,
accounts, or schema migrations.

Machine-readable schemas are in `schemas/content-update/`.

## Logical Package

```text
content-update-0002/
├── manifest.json
├── checksums.json
├── translations.jsonl
├── routes.jsonl
├── route-points.jsonl
└── studies.jsonl
```

Only files declared by `manifest.json` are allowed. Files are regular files,
not symlinks. Version 1 uses an unpacked directory; a future archive transport
must preserve this logical structure and additionally reject duplicate archive
entries, absolute paths, traversal, links, and decompression beyond the same
limits.

## Encoding and Determinism

- Manifest and checksum documents are canonical UTF-8 JSON.
- Payloads are UTF-8 JSON Lines using LF (`U+000A`) only.
- One compact JSON object occupies each non-empty line.
- JSON integers are base-10 integers. Missing optional values are represented
  as JSON `null`, never magic strings.
- Payload order is significant and preserved. Route `sequence_order` is a
  non-negative integer unique within a route.
- Producers sort object keys and payload filenames lexicographically for
  reproducible artifacts. Consumers must not depend on JSON object key order.

## Checksums

Each payload SHA-256 is calculated over its exact file bytes and represented as
64 lowercase hexadecimal characters.

The aggregate checksum is SHA-256 over payload entries sorted by path. For each
entry, feed these exact bytes:

```text
path + NUL + lowercase_payload_sha256 + LF
```

`manifest.json` and `checksums.json` must contain the same aggregate checksum.
`checksums.json.files` must exactly match the declared payload paths. All
checksums and operation counts are validated before a write transaction.

## Versions and Ordering

- `manifest_version` version-controls this contract and is currently `1`.
- `content_version` is a positive, monotonically increasing platform-wide
  integer.
- `update_id` is a stable Rhelo-owned identifier.
- A `(content_version, update_id)` pair is applied at most once.
- Conflicting duplicate IDs or versions and content-version rollback are
  rejected.
- Packages are discovered and applied in ascending `content_version`.
- Schema compatibility is bounded by `minimum_schema_version` and optional
  `maximum_schema_version`.
- `minimum_app_version` is optional and contains a numeric semantic version.

## Identifiers

- Canonical verse identifiers are the existing OSIS-style `verses_base.id`
  values such as `GEN.1.1` and `JHN.3.16`.
- Translation codes remain stable provider/database codes within payloads.
  Rhelo-owned catalogue IDs remain the public application identifiers defined
  in `docs/TRANSLATION_CONTRACT.md`.
- Route IDs are stable Rhelo-owned ASCII identifiers and are never array
  indexes or display titles.
- Source, licence, and attribution references are stable IDs whose legal
  records must be available to the producing release process.

## Version 1 Allowlist

| Table | JSONL fields | Validation |
| --- | --- | --- |
| `verse_translations` | `verse_id`, `translation_code`, `text` | Verse exists; code is non-empty ASCII ID; text is non-empty; duplicate composite key rejected |
| `geography_routes` | `route_id`, `title`, `description` | Stable ID; non-empty title; insert-only |
| `route_points` | `route_id`, `sequence_order`, `latitude`, `longitude`, `place_name`, `associated_verse_id` | New route is in same package; at least two points; unique order; valid coordinate; optional verse exists |
| `commentaries` | `commentary_id`, `verse_id`, `text` | Stable resource ID; verse exists; non-empty text; duplicate key rejected |
| `cross_references` | `from_verse`, `to_verse`, `votes` | Both verses exist; duplicate pair rejected |

Version 1 supports only `insert_only`. It supports no payload deletion,
arbitrary SQL, schema changes, generic table names, or static FTS rebuild.
Controlled source-owned upsert requires a later contract and schema with
explicit ownership metadata.

## Denied Data

Unknown tables are rejected. User-owned tables are always denied, including:

- `sessions`
- `session_documents`
- `sessions_fts`
- `chat_history`
- settings and preferences
- transcripts, notes, and user-created documents

`sessions_fts` must never appear in `fts_rebuild`.

## Limits

- Maximum unpacked package size: 16 MiB.
- Maximum payload size: 8 MiB.
- Maximum JSONL line size: 1 MiB.
- Maximum rows per package: 100,000.
- Payload paths contain one safe filename component and end in `.jsonl`.

## Transaction and Failure Rules

Validation of structure, versions, paths, checksums, aggregate checksum, file
sizes, JSON shape, allowlist, and declared counts occurs before mutation.

Application uses one immediate SQLite transaction per package:

1. Revalidate schema and current content version.
2. Validate and insert typed rows.
3. Verify actual inserted count.
4. Run targeted orphan checks.
5. Insert the `content_updates` record.
6. Commit.

Any failure rolls back all payload rows and the tracking record. Existing
content remains available, and retrying the unchanged package on a later launch
is safe.

## Baselines and Clean Installs

`baseline.json` describes content already present in a seed using expected
static table counts, complete translation-code counts, required route IDs, and
a deterministic fingerprint. It deliberately does not hash a mutable 281 MB
database file.

A clean install copies its seed, migrates schema, records the recognized
baseline, and applies only newer packages. An existing installation keeps its
database, migrates it, recognizes the same static baseline despite user-owned
rows, and applies only newer packages. Unknown or materially different
baselines are not marked current.

## Cross-Platform Test Fixture

`frontend/src-tauri/tests/fixtures/content-update-0002/` contains synthetic,
unlicensed text with one translation row, one route, two ordered points, and
one study row. Mobile implementations should reproduce the fixture semantics
and rejection cases, but must not copy Desktop filesystem or UI code.
