import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";

const repositoryRoot = resolve("..");
const fixtureRoot = resolve("src-tauri", "tests", "fixtures", "content-update-0002");
const schemaRoot = resolve(repositoryRoot, "schemas", "content-update");

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function aggregateChecksum(files) {
  const hash = createHash("sha256");
  for (const [path, checksum] of Object.entries(files).sort(([left], [right]) => left.localeCompare(right))) {
    hash.update(path);
    hash.update("\0");
    hash.update(checksum);
    hash.update("\n");
  }
  return hash.digest("hex");
}

test("machine-readable content schemas are valid JSON Schema documents", () => {
  for (const filename of [
    "manifest-v1.schema.json",
    "checksums-v1.schema.json",
    "baseline-v1.schema.json",
  ]) {
    const schema = readJson(resolve(schemaRoot, filename));
    assert.equal(schema.$schema, "https://json-schema.org/draft/2020-12/schema");
    assert.equal(schema.type, "object");
    assert.equal(schema.additionalProperties, false);
  }
});

test("synthetic package payload and aggregate checksums are deterministic", () => {
  const manifest = readJson(resolve(fixtureRoot, "manifest.json"));
  const checksums = readJson(resolve(fixtureRoot, "checksums.json"));
  assert.equal(manifest.manifest_version, 1);
  assert.equal(manifest.update_id, "test-content-update-0002");
  assert.equal(manifest.content_version, 2);
  assert.equal(manifest.expected_inserted_rows, 5);
  assert.deepEqual(manifest.fts_rebuild, []);

  for (const payload of manifest.payload_files) {
    const bytes = readFileSync(resolve(fixtureRoot, payload.path));
    assert.equal(sha256(bytes), payload.sha256);
    assert.equal(checksums.files[payload.path], payload.sha256);
  }
  assert.equal(aggregateChecksum(checksums.files), manifest.package_sha256);
  assert.equal(checksums.package_sha256, manifest.package_sha256);
});

test("test package is not included in production Tauri resources", () => {
  const tauriConfig = readJson(resolve("src-tauri", "tauri.conf.json"));
  const resources = tauriConfig.bundle.resources;
  assert.equal(resources["resources/content-updates"], "content-updates");
  assert.equal(
    Object.keys(resources).some((path) => path.includes("tests/fixtures")),
    false,
  );

  const baseline = readJson(resolve("src-tauri", "resources", "content-updates", "baseline.json"));
  assert.equal(baseline.content_version, 1);
  assert.equal(
    JSON.stringify(baseline).includes("test_phase3"),
    false,
  );
});

test("optional startup failures use structured IPC and a dismissible warning", () => {
  const rustHost = readFileSync(resolve("src-tauri", "src", "lib.rs"), "utf8");
  const apiClient = readFileSync(resolve("src", "lib", "api.ts"), "utf8");
  const page = readFileSync(resolve("src", "app", "page.tsx"), "utf8");
  assert.match(rustHost, /fetch_database_startup_status/);
  assert.match(apiClient, /fetchDatabaseStartupStatus/);
  assert.match(page, /contentUpdateWarning/);
  assert.match(page, /Dismiss content update warning/);
});

test("legacy seed tooling cannot stamp an unmigrated database as schema v3", () => {
  const finalizer = readFileSync(
    resolve(repositoryRoot, "scripts", "finalize_seed_database.py"),
    "utf8",
  );
  assert.match(finalizer, /GENERATED_SEED_SCHEMA_VERSION = 1/);
  assert.match(finalizer, /Runtime migrations, not this finalizer/);
  assert.doesNotMatch(finalizer, /PRAGMA user_version = \{runtime_schema_version\}/);
});
