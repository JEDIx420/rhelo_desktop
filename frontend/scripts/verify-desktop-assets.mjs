import { readFileSync, statSync } from "node:fs";
import { resolve } from "node:path";

import { parseCargoLockPackageVersion } from "./desktop-version.mjs";

const repositoryRoot = resolve("..");
const schemaVersionPath = resolve(repositoryRoot, "schema-version.txt");
const databasePath = resolve(repositoryRoot, "rhelo.db");
const minimumRuntimeMigratableSchemaVersion = 1;
const contentBaselinePath = resolve("src-tauri", "resources", "content-updates", "baseline.json");
const fontFiles = [
  "NotoSans-Regular.ttf",
  "NotoSansHebrew-Regular.ttf",
  "NotoSansDevanagari-Regular.ttf",
  "NotoSansTelugu-Regular.ttf",
  "NotoSansMalayalam-Regular.ttf",
  "NotoSansTamil-Regular.ttf",
];
const requiredAssets = [
  { path: databasePath, minimumBytes: 1_000_000, label: "Rhelo database" },
  { path: resolve(repositoryRoot, "ggml-base.bin"), minimumBytes: 1_000_000, label: "Whisper model" },
  { path: contentBaselinePath, minimumBytes: 100, label: "Bundled content baseline" },
  ...fontFiles.map((filename) => ({
    path: resolve("src-tauri", "resources", "fonts", filename),
    minimumBytes: 10_000,
    label: `PDF font ${filename}`,
  })),
];

const failures = [];

try {
  const packageManifest = JSON.parse(readFileSync(resolve("package.json"), "utf8"));
  const packageLock = JSON.parse(readFileSync(resolve("package-lock.json"), "utf8"));
  const tauriConfig = JSON.parse(readFileSync(resolve("src-tauri", "tauri.conf.json"), "utf8"));
  const cargoManifest = readFileSync(resolve("src-tauri", "Cargo.toml"), "utf8");
  const cargoLock = readFileSync(resolve("src-tauri", "Cargo.lock"), "utf8");
  const cargoPackage = cargoManifest.match(/^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m);
  const versions = {
    "package.json": packageManifest.version,
    "package-lock.json": packageLock.version,
    "package-lock.json root package": packageLock.packages?.[""]?.version,
    "Cargo.toml": cargoPackage?.[1],
    "Cargo.lock": parseCargoLockPackageVersion(cargoLock, "rhelo"),
    "tauri.conf.json": tauriConfig.version,
  };
  const expectedVersion = packageManifest.version;
  for (const [source, version] of Object.entries(versions)) {
    if (!version) failures.push(`Desktop version is missing from ${source}`);
    else if (version !== expectedVersion) {
      failures.push(`Desktop version mismatch: ${source} is ${version}, expected ${expectedVersion}`);
    }
  }
} catch (error) {
  failures.push(`Desktop application versions could not be validated: ${error}`);
}

for (const asset of requiredAssets) {
  try {
    if (statSync(asset.path).size < asset.minimumBytes) failures.push(`${asset.label} is empty or incomplete: ${asset.path}`);
  } catch {
    failures.push(`${asset.label} is missing: ${asset.path}`);
  }
}

try {
  const expectedSchemaVersion = Number.parseInt(readFileSync(schemaVersionPath, "ascii").trim(), 10);
  if (!Number.isSafeInteger(expectedSchemaVersion) || expectedSchemaVersion < 0) {
    failures.push(`${schemaVersionPath} must contain a non-negative integer`);
  } else {
    const databaseHeader = readFileSync(databasePath).subarray(0, 100);
    if (databaseHeader.toString("ascii", 0, 16) !== "SQLite format 3\u0000") {
      failures.push(`Rhelo database does not have a valid SQLite header: ${databasePath}`);
    } else {
      const bundledSchemaVersion = databaseHeader.readUInt32BE(60);
      if (
        bundledSchemaVersion < minimumRuntimeMigratableSchemaVersion
        || bundledSchemaVersion > expectedSchemaVersion
      ) {
        failures.push(
          `Bundled database user_version ${bundledSchemaVersion} is outside the runtime-supported range ${minimumRuntimeMigratableSchemaVersion}-${expectedSchemaVersion}`
        );
      }
    }
  }
} catch (error) {
  failures.push(`Bundled database schema version could not be validated: ${error}`);
}

try {
  const tauriConfig = JSON.parse(readFileSync(resolve("src-tauri", "tauri.conf.json"), "utf8"));
  const resources = tauriConfig?.bundle?.resources;
  if (resources?.["../../rhelo.db"] !== "rhelo.db") failures.push("Tauri must bundle rhelo.db at the resource root");
  if (resources?.["../../ggml-base.bin"] !== "ggml-base.bin") failures.push("Tauri must bundle the Whisper model at the resource root");
  if (resources?.["resources/content-updates"] !== "content-updates") failures.push("Tauri must bundle the content baseline directory");
  for (const filename of fontFiles) {
    if (resources?.[`resources/fonts/${filename}`] !== `fonts/${filename}`) {
      failures.push(`Tauri must bundle ${filename} in the fonts resource directory`);
    }
  }
  if ("externalBin" in (tauriConfig?.bundle ?? {})) failures.push("The pure Rust build must not configure a Python sidecar");
  if (tauriConfig?.app?.windows?.[0]?.dragDropEnabled !== false) failures.push("dragDropEnabled must stay false for internal macOS drag-and-drop");
} catch (error) {
  failures.push(`Tauri resource configuration could not be validated: ${error}`);
}

try {
  const rustHost = readFileSync(resolve("src-tauri", "src", "lib.rs"), "utf8");
  const apiClient = readFileSync(resolve("src", "lib", "api.ts"), "utf8");
  if (rustHost.includes("spawn_sidecar")) failures.push("Rust still contains sidecar lifecycle code");
  if (apiClient.includes("127.0.0.1:5050") || apiClient.includes("fetchRhelo")) failures.push("The frontend still contains Python HTTP fallback logic");
  if (!apiClient.includes('invokeDesktop<SessionPDFExportResult>("export_and_save_session_pdf"')) failures.push("PDF export must use the unified native save IPC command");
} catch (error) {
  failures.push(`Native architecture validation failed: ${error}`);
}

if (failures.length) {
  console.error(`Desktop asset verification failed:\n- ${failures.join("\n- ")}`);
  process.exit(1);
}

console.log("Desktop assets are ready for the pure Rust build.");
