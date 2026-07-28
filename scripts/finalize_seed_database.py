"""Finalize the generated seed database for desktop packaging."""

from pathlib import Path
import sqlite3


ROOT = Path(__file__).resolve().parent.parent
DATABASE_PATH = ROOT / "rhelo.db"
SCHEMA_VERSION_PATH = ROOT / "schema-version.txt"
GENERATED_SEED_SCHEMA_VERSION = 1


def read_schema_version() -> int:
    value = SCHEMA_VERSION_PATH.read_text(encoding="ascii").strip()
    version = int(value)
    if version < 0:
        raise ValueError("Schema version must be non-negative.")
    return version


def main() -> None:
    runtime_schema_version = read_schema_version()
    if runtime_schema_version < GENERATED_SEED_SCHEMA_VERSION:
        raise RuntimeError(
            "Runtime schema support is older than the generated seed schema."
        )
    if not DATABASE_PATH.exists():
        raise FileNotFoundError(f"Seed database not found: {DATABASE_PATH}")

    connection = sqlite3.connect(DATABASE_PATH)
    try:
        # Migrations 000-012 currently generate the production v1 schema.
        # Runtime migrations, not this finalizer, perform the v2/v3 upgrades.
        connection.execute(f"PRAGMA user_version = {GENERATED_SEED_SCHEMA_VERSION}")
        connection.commit()
        stored_version = connection.execute("PRAGMA user_version").fetchone()[0]
        if stored_version != GENERATED_SEED_SCHEMA_VERSION:
            raise RuntimeError(
                "Seed database version verification failed: "
                f"expected {GENERATED_SEED_SCHEMA_VERSION}, got {stored_version}"
            )
    finally:
        connection.close()

    print(
        "Seed database finalized at schema version "
        f"{GENERATED_SEED_SCHEMA_VERSION}; runtime supports "
        f"schema version {runtime_schema_version}."
    )


if __name__ == "__main__":
    main()
