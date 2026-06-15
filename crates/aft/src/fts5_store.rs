//! Versioned FTS5 SQLite store for the opt-in FTS5 side feature.
//!
//! This module replaces the single-table spike in `fts5_experimental.rs` with
//! a production-shaped, versioned multi-table schema. The schema is designed
//! for:
//!
//! - **Separate symbol/body/path FTS tables** — avoids a single universal
//!   trigram table that performs poorly for code.
//! - **Exact SQL lookup** — symbol names use normal SQL indexes for exact
//!   and prefix matches.
//! - **Versioned schema** — schema version stored in `fts5_meta` allows safe
//!   upgrades and rebuild detection.
//! - **Transactional consistency** — regular tables and FTS tables are
//!   maintained transactionally.
//!
//! ## Schema v1
//!
//! ```text
//! fts5_meta            — schema version, build metadata
//! fts5_files           — file paths, hashes, mtime
//! fts5_symbols         — symbol names, kinds, ranges, file references
//! fts5_symbols_fts     — FTS5 virtual table for symbol name search
//! fts5_symbol_bodies_fts — FTS5 virtual table for symbol body search
//! fts5_paths_fts       — FTS5 virtual table for file path search
//! ```

use crate::fts5_experimental::check_fts5_available;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Current schema version. Bump when adding/removing tables or columns.
pub const SCHEMA_VERSION: i64 = 4;

/// Maximum characters stored per symbol body in the FTS table.
/// Bodies exceeding this are truncated; truncation is recorded.
const DEFAULT_MAX_BODY_CHARS: usize = 2000;

/// Maximum lines stored per symbol body.
const DEFAULT_MAX_BODY_LINES: usize = 60;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from FTS5 store operations.
#[derive(Debug)]
pub enum Fts5StoreError {
    /// SQLite error.
    Sqlite(rusqlite::Error),
    /// I/O error.
    Io(std::io::Error),
    /// FTS5 not available in this SQLite build.
    Fts5Unavailable,
    /// Schema version mismatch — requires rebuild.
    SchemaMismatch { expected: i64, found: i64 },
    /// Generic error message.
    Other(String),
}

impl std::fmt::Display for Fts5StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Fts5Unavailable => write!(f, "FTS5 is not available in this SQLite build"),
            Self::SchemaMismatch { expected, found } => {
                write!(
                    f,
                    "schema version mismatch: expected v{expected}, found v{found}"
                )
            }
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Fts5StoreError {}

impl From<rusqlite::Error> for Fts5StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for Fts5StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// File record
// ---------------------------------------------------------------------------

/// A file tracked in the FTS5 index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub hash: String,
    pub mtime_secs: i64,
    pub indexed_at: i64,
    /// File size in bytes at indexing time (v2+).
    pub size_bytes: u64,
    /// Index generation that produced this record (v2+).
    pub generation: i64,
}

// ---------------------------------------------------------------------------
// Symbol record
// ---------------------------------------------------------------------------

/// A symbol tracked in the FTS5 index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
    pub indexed_at: i64,
    /// Qualified name path (v3+), e.g. "MyClass::my_method".
    /// Empty string for top-level symbols.
    pub name_path: String,
    /// Body content hash (v3+), for content identity without re-reading source.
    pub body_hash: String,
    /// Duplicate index within same file/name/kind (v3+).
    /// 0 for unique symbols, 1+ for duplicates.
    pub duplicate_index: i32,
}

/// A chunk record — a stable, addressable span of source content.
///
/// Chunks are the atomic unit for search, enrichment, and read sidecars.
/// Each chunk carries enough metadata for precise retrieval and deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRecord {
    pub id: i64,
    pub file_id: i64,
    /// Start line (1-based, inclusive).
    pub start_line: u32,
    /// End line (1-based, inclusive).
    pub end_line: u32,
    /// Content type: "symbol", "block", "paragraph", "config", "heading", "summary".
    pub chunk_kind: String,
    /// Symbol name if this chunk came from a tree-sitter symbol, empty otherwise.
    pub symbol_name: String,
    /// Symbol kind if applicable (function, class, struct, etc.), empty otherwise.
    pub symbol_kind: String,
    /// Content hash of the chunk body.
    pub content_hash: String,
    /// Truncated body text.
    pub body: String,
    /// Indexed at timestamp (epoch seconds).
    pub indexed_at: i64,
}

/// Chunk kind constants.
pub mod chunk_kind {
    pub const SYMBOL: &str = "symbol";
    pub const BLOCK: &str = "block";
    pub const PARAGRAPH: &str = "paragraph";
    pub const CONFIG: &str = "config";
    pub const HEADING: &str = "heading";
    pub const SUMMARY: &str = "summary";
}

// ---------------------------------------------------------------------------
// Search result
// ---------------------------------------------------------------------------

/// A single search result from the FTS5 store.
#[derive(Debug, Clone)]
pub struct Fts5SearchResult {
    pub symbol_id: i64,
    pub file_id: i64,
    pub file_path: String,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub snippet: String,
    pub rank: f64,
    pub lane: String,
    /// Qualified name path (v3+).
    pub name_path: String,
    /// Duplicate index within same file/name/kind (v3+).
    pub duplicate_index: i32,
}

// ---------------------------------------------------------------------------
// Fts5Store
// ---------------------------------------------------------------------------

/// Versioned FTS5 SQLite store for the opt-in FTS5 side feature.
///
/// Manages schema creation, version checking, and CRUD operations for the
/// multi-table FTS5 schema. Each project root gets its own SQLite database
/// file.
pub struct Fts5Store {
    /// The SQLite connection (exposed for query planner access).
    pub conn: Connection,
    db_path: PathBuf,
}

impl Fts5Store {
    /// Open or create an FTS5 store at the given path.
    ///
    /// If the database file doesn't exist, it's created with the current
    /// schema. If it exists but has a different schema version, returns
    /// `SchemaMismatch` — the caller should rebuild.
    pub fn open(path: &Path) -> Result<Self, Fts5StoreError> {
        if !check_fts5_available() {
            return Err(Fts5StoreError::Fts5Unavailable);
        }

        let path = path.to_path_buf();
        let is_new = !path.exists();

        let conn = Connection::open(&path)?;

        // Enable WAL mode for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        // Foreign keys must be enabled explicitly in SQLite.
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let store = Self {
            conn,
            db_path: path,
        };

        if is_new {
            store.create_schema()?;
        } else {
            store.check_schema_version()?;
        }

        Ok(store)
    }

    /// Open an in-memory FTS5 store (for tests and diagnostics).
    pub fn open_in_memory() -> Result<Self, Fts5StoreError> {
        if !check_fts5_available() {
            return Err(Fts5StoreError::Fts5Unavailable);
        }

        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let store = Self {
            conn,
            db_path: PathBuf::from(":memory:"),
        };

        store.create_schema()?;

        Ok(store)
    }

    /// Get the schema version stored in the database.
    pub fn schema_version(&self) -> Result<i64, Fts5StoreError> {
        let value: String = self.conn.query_row(
            "SELECT value FROM fts5_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        value
            .parse::<i64>()
            .map_err(|e| Fts5StoreError::Other(format!("invalid schema version in database: {e}")))
    }

    /// Get the database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Get the database file size in bytes (0 for in-memory).
    pub fn db_size_bytes(&self) -> u64 {
        if self.db_path.as_os_str() == ":memory:" {
            return 0;
        }
        std::fs::metadata(&self.db_path)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    // -----------------------------------------------------------------------
    // Schema management
    // -----------------------------------------------------------------------

    /// Create the v2 schema.
    ///
    /// V2 adds `size_bytes` and `generation` columns to `fts5_files` for
    /// content-hash-based freshness and generation tracking.
    fn create_schema(&self) -> Result<(), Fts5StoreError> {
        self.conn.execute_batch(
            "
            -- Metadata table: schema version and build info
            CREATE TABLE IF NOT EXISTS fts5_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- File tracking table (v2: adds size_bytes, generation)
            CREATE TABLE IF NOT EXISTS fts5_files (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                path        TEXT NOT NULL UNIQUE,
                hash        TEXT NOT NULL,
                mtime_secs  INTEGER NOT NULL,
                indexed_at  INTEGER NOT NULL,
                size_bytes  INTEGER NOT NULL DEFAULT 0,
                generation  INTEGER NOT NULL DEFAULT 1
            );

            -- Symbol tracking table (v3: adds name_path, body_hash, duplicate_index)
            CREATE TABLE IF NOT EXISTS fts5_symbols (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id         INTEGER NOT NULL REFERENCES fts5_files(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                kind            TEXT NOT NULL,
                start_line      INTEGER NOT NULL,
                end_line        INTEGER NOT NULL,
                body            TEXT NOT NULL,
                indexed_at      INTEGER NOT NULL,
                name_path       TEXT NOT NULL DEFAULT '',
                body_hash       TEXT NOT NULL DEFAULT '',
                duplicate_index INTEGER NOT NULL DEFAULT 0
            );

            -- Indexes for exact/prefix symbol lookup
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON fts5_symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON fts5_symbols(file_id);
            CREATE INDEX IF NOT EXISTS idx_symbols_kind ON fts5_symbols(kind);

            -- Chunk tracking table (v4): stable addressable spans of source content
            CREATE TABLE IF NOT EXISTS fts5_chunks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id     INTEGER NOT NULL REFERENCES fts5_files(id) ON DELETE CASCADE,
                start_line  INTEGER NOT NULL,
                end_line    INTEGER NOT NULL,
                chunk_kind  TEXT NOT NULL,
                symbol_name TEXT NOT NULL DEFAULT '',
                symbol_kind TEXT NOT NULL DEFAULT '',
                content_hash TEXT NOT NULL,
                body        TEXT NOT NULL,
                indexed_at  INTEGER NOT NULL
            );

            -- Indexes for chunk lookup
            CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON fts5_chunks(file_id);
            CREATE INDEX IF NOT EXISTS idx_chunks_chunk_kind ON fts5_chunks(chunk_kind);
            CREATE INDEX IF NOT EXISTS idx_chunks_symbol_name ON fts5_chunks(symbol_name);

            -- FTS5 virtual table for symbol name search (unicode61 tokenizer)
            CREATE VIRTUAL TABLE IF NOT EXISTS fts5_symbols_fts USING fts5(
                name,
                kind,
                content='fts5_symbols',
                content_rowid='id',
                tokenize='unicode61'
            );

            -- Triggers to keep fts5_symbols_fts in sync with fts5_symbols
            CREATE TRIGGER IF NOT EXISTS fts5_symbols_ai AFTER INSERT ON fts5_symbols BEGIN
                INSERT INTO fts5_symbols_fts(rowid, name, kind)
                VALUES (new.id, new.name, new.kind);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_symbols_ad AFTER DELETE ON fts5_symbols BEGIN
                INSERT INTO fts5_symbols_fts(fts5_symbols_fts, rowid, name, kind)
                VALUES ('delete', old.id, old.name, old.kind);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_symbols_au AFTER UPDATE ON fts5_symbols BEGIN
                INSERT INTO fts5_symbols_fts(fts5_symbols_fts, rowid, name, kind)
                VALUES ('delete', old.id, old.name, old.kind);
                INSERT INTO fts5_symbols_fts(rowid, name, kind)
                VALUES (new.id, new.name, new.kind);
            END;

            -- FTS5 virtual table for symbol body search (trigram tokenizer)
            CREATE VIRTUAL TABLE IF NOT EXISTS fts5_symbol_bodies_fts USING fts5(
                body,
                content='fts5_symbols',
                content_rowid='id',
                tokenize='trigram'
            );

            -- Triggers to keep fts5_symbol_bodies_fts in sync
            CREATE TRIGGER IF NOT EXISTS fts5_bodies_ai AFTER INSERT ON fts5_symbols BEGIN
                INSERT INTO fts5_symbol_bodies_fts(rowid, body)
                VALUES (new.id, new.body);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_bodies_ad AFTER DELETE ON fts5_symbols BEGIN
                INSERT INTO fts5_symbol_bodies_fts(fts5_symbol_bodies_fts, rowid, body)
                VALUES ('delete', old.id, old.body);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_bodies_au AFTER UPDATE ON fts5_symbols BEGIN
                INSERT INTO fts5_symbol_bodies_fts(fts5_symbol_bodies_fts, rowid, body)
                VALUES ('delete', old.id, old.body);
                INSERT INTO fts5_symbol_bodies_fts(rowid, body)
                VALUES (new.id, new.body);
            END;

            -- FTS5 virtual table for file path search (trigram tokenizer)
            CREATE VIRTUAL TABLE IF NOT EXISTS fts5_paths_fts USING fts5(
                path,
                content='fts5_files',
                content_rowid='id',
                tokenize='trigram'
            );

            -- Triggers to keep fts5_paths_fts in sync
            CREATE TRIGGER IF NOT EXISTS fts5_paths_ai AFTER INSERT ON fts5_files BEGIN
                INSERT INTO fts5_paths_fts(rowid, path)
                VALUES (new.id, new.path);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_paths_ad AFTER DELETE ON fts5_files BEGIN
                INSERT INTO fts5_paths_fts(fts5_paths_fts, rowid, path)
                VALUES ('delete', old.id, old.path);
            END;

            CREATE TRIGGER IF NOT EXISTS fts5_paths_au AFTER UPDATE ON fts5_files BEGIN
                INSERT INTO fts5_paths_fts(fts5_paths_fts, rowid, path)
                VALUES ('delete', old.id, old.path);
                INSERT INTO fts5_paths_fts(rowid, path)
                VALUES (new.id, new.path);
            END;
            ",
        )?;

        // Set schema version
        self.conn.execute(
            "INSERT OR REPLACE INTO fts5_meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;

        // Set build timestamp
        let now = now_secs();
        self.conn.execute(
            "INSERT OR REPLACE INTO fts5_meta (key, value) VALUES ('created_at', ?1)",
            params![now.to_string()],
        )?;

        Ok(())
    }

    /// Check that the schema version matches the expected version.
    fn check_schema_version(&self) -> Result<(), Fts5StoreError> {
        match self.schema_version() {
            Ok(version) if version == SCHEMA_VERSION => Ok(()),
            Ok(version) => Err(Fts5StoreError::SchemaMismatch {
                expected: SCHEMA_VERSION,
                found: version,
            }),
            Err(_) => Err(Fts5StoreError::SchemaMismatch {
                expected: SCHEMA_VERSION,
                found: 0,
            }),
        }
    }

    /// Destroy and recreate the schema (full rebuild).
    pub fn rebuild(&self) -> Result<(), Fts5StoreError> {
        self.conn.execute_batch(
            "
            DROP TRIGGER IF EXISTS fts5_symbols_ai;
            DROP TRIGGER IF EXISTS fts5_symbols_ad;
            DROP TRIGGER IF EXISTS fts5_symbols_au;
            DROP TRIGGER IF EXISTS fts5_bodies_ai;
            DROP TRIGGER IF EXISTS fts5_bodies_ad;
            DROP TRIGGER IF EXISTS fts5_bodies_au;
            DROP TRIGGER IF EXISTS fts5_paths_ai;
            DROP TRIGGER IF EXISTS fts5_paths_ad;
            DROP TRIGGER IF EXISTS fts5_paths_au;
            DROP TABLE IF EXISTS fts5_symbols_fts;
            DROP TABLE IF EXISTS fts5_symbol_bodies_fts;
            DROP TABLE IF EXISTS fts5_paths_fts;
            DROP TABLE IF EXISTS fts5_symbols;
            DROP TABLE IF EXISTS fts5_files;
            DROP TABLE IF EXISTS fts5_meta;
            ",
        )?;

        self.create_schema()
    }

    // -----------------------------------------------------------------------
    // File operations
    // -----------------------------------------------------------------------

    /// Upsert a file record. Returns the file ID.
    ///
    /// Uses `RETURNING id` to get the correct row ID for both INSERT and
    /// UPDATE paths. The previous `last_insert_rowid()` approach returned a
    /// stale value when `ON CONFLICT` triggered an UPDATE.
    pub fn upsert_file(
        &self,
        path: &str,
        hash: &str,
        mtime_secs: i64,
        size_bytes: u64,
        generation: i64,
    ) -> Result<i64, Fts5StoreError> {
        let now = now_secs();
        let file_id: i64 = self.conn.query_row(
            "INSERT INTO fts5_files (path, hash, mtime_secs, indexed_at, size_bytes, generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
               hash = excluded.hash,
               mtime_secs = excluded.mtime_secs,
               indexed_at = excluded.indexed_at,
               size_bytes = excluded.size_bytes,
               generation = excluded.generation
             RETURNING id",
            params![path, hash, mtime_secs, now, size_bytes as i64, generation],
            |row| row.get(0),
        )?;
        Ok(file_id)
    }

    /// Delete a file and all its symbols and chunks (cascade).
    pub fn delete_file(&self, file_id: i64) -> Result<(), Fts5StoreError> {
        self.conn.execute(
            "DELETE FROM fts5_chunks WHERE file_id = ?1",
            params![file_id],
        )?;
        self.conn
            .execute("DELETE FROM fts5_files WHERE id = ?1", params![file_id])?;
        Ok(())
    }

    /// Delete a file by path.
    pub fn delete_file_by_path(&self, path: &str) -> Result<(), Fts5StoreError> {
        self.conn
            .execute("DELETE FROM fts5_files WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Get a file record by path.
    pub fn get_file_by_path(&self, path: &str) -> Result<Option<FileRecord>, Fts5StoreError> {
        let result = self
            .conn
            .query_row(
                "SELECT id, path, hash, mtime_secs, indexed_at, size_bytes, generation FROM fts5_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(FileRecord {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        hash: row.get(2)?,
                        mtime_secs: row.get(3)?,
                        indexed_at: row.get(4)?,
                        size_bytes: row.get(5)?,
                        generation: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Get a file record by ID.
    pub fn get_file_by_id(&self, file_id: i64) -> Result<Option<FileRecord>, Fts5StoreError> {
        let result = self
            .conn
            .query_row(
                "SELECT id, path, hash, mtime_secs, indexed_at, size_bytes, generation FROM fts5_files WHERE id = ?1",
                params![file_id],
                |row| {
                    Ok(FileRecord {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        hash: row.get(2)?,
                        mtime_secs: row.get(3)?,
                        indexed_at: row.get(4)?,
                        size_bytes: row.get(5)?,
                        generation: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Get all file records.
    pub fn get_all_files(&self) -> Result<Vec<FileRecord>, Fts5StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, hash, mtime_secs, indexed_at, size_bytes, generation FROM fts5_files ORDER BY path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FileRecord {
                id: row.get(0)?,
                path: row.get(1)?,
                hash: row.get(2)?,
                mtime_secs: row.get(3)?,
                indexed_at: row.get(4)?,
                size_bytes: row.get(5)?,
                generation: row.get(6)?,
            })
        })?;
        let files = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(files)
    }

    /// Count files in the index.
    pub fn file_count(&self) -> Result<usize, Fts5StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fts5_files", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Get file paths with stale content (file on disk differs from indexed).
    ///
    /// Uses content hash and size for freshness detection, not just mtime.
    /// This catches rapid rewrites with the same mtime (e.g.,在同一秒内的编辑).
    /// Returns (path, reason) for each stale file.
    pub fn stale_files(&self, project_root: &Path) -> Result<Vec<StaleFileInfo>, Fts5StoreError> {
        let files = self.get_all_files()?;
        let mut stale = Vec::new();

        for file in &files {
            let abs_path = project_root.join(&file.path);
            match std::fs::metadata(&abs_path) {
                Ok(metadata) => {
                    let current_size = metadata.len();
                    let current_mtime = metadata
                        .modified()
                        .unwrap_or(UNIX_EPOCH)
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    // Check size first (cheap), then mtime, then content hash
                    if current_size != file.size_bytes as u64 {
                        stale.push(StaleFileInfo {
                            path: file.path.clone(),
                            indexed_mtime: file.mtime_secs,
                            current_mtime,
                        });
                    } else if current_mtime != file.mtime_secs {
                        // Size matches but mtime differs — verify with content hash
                        if let Ok(source) = std::fs::read(&abs_path) {
                            let current_hash = blake3::hash(&source).to_hex().to_string();
                            if current_hash != file.hash {
                                stale.push(StaleFileInfo {
                                    path: file.path.clone(),
                                    indexed_mtime: file.mtime_secs,
                                    current_mtime,
                                });
                            }
                        }
                    }
                }
                Err(_) => {
                    // File no longer exists on disk
                    stale.push(StaleFileInfo {
                        path: file.path.clone(),
                        indexed_mtime: file.mtime_secs,
                        current_mtime: -1, // Sentinel: file deleted
                    });
                }
            }
        }

        Ok(stale)
    }

    // -----------------------------------------------------------------------
    // Symbol operations
    // -----------------------------------------------------------------------

    /// Upsert a symbol record. Returns the symbol ID.
    pub fn upsert_symbol(
        &self,
        file_id: i64,
        name: &str,
        kind: &str,
        start_line: u32,
        end_line: u32,
        body: &str,
        name_path: &str,
        body_hash: &str,
    ) -> Result<i64, Fts5StoreError> {
        let now = now_secs();
        let truncated_body = truncate_body(body, DEFAULT_MAX_BODY_CHARS, DEFAULT_MAX_BODY_LINES);

        // Check if a symbol with this name and file already exists
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM fts5_symbols WHERE file_id = ?1 AND name = ?2",
                params![file_id, name],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            self.conn.execute(
                "UPDATE fts5_symbols SET kind = ?1, start_line = ?2, end_line = ?3,
                 body = ?4, indexed_at = ?5, name_path = ?6, body_hash = ?7 WHERE id = ?8",
                params![
                    kind,
                    start_line,
                    end_line,
                    truncated_body,
                    now,
                    name_path,
                    body_hash,
                    id
                ],
            )?;
            Ok(id)
        } else {
            // Compute duplicate_index: count existing symbols with same name+file
            let dup_count: i32 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM fts5_symbols WHERE file_id = ?1 AND name = ?2",
                    params![file_id, name],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            self.conn.execute(
                "INSERT INTO fts5_symbols (file_id, name, kind, start_line, end_line, body, indexed_at, name_path, body_hash, duplicate_index)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![file_id, name, kind, start_line, end_line, truncated_body, now, name_path, body_hash, dup_count],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    /// Delete all symbols for a file.
    pub fn delete_symbols_for_file(&self, file_id: i64) -> Result<(), Fts5StoreError> {
        self.conn.execute(
            "DELETE FROM fts5_symbols WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(())
    }

    /// Get a symbol record by ID.
    pub fn get_symbol(&self, symbol_id: i64) -> Result<Option<SymbolRecord>, Fts5StoreError> {
        let result = self
            .conn
            .query_row(
                "SELECT id, file_id, name, kind, start_line, end_line, body, indexed_at,
                        name_path, body_hash, duplicate_index
                 FROM fts5_symbols WHERE id = ?1",
                params![symbol_id],
                |row| {
                    Ok(SymbolRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        name: row.get(2)?,
                        kind: row.get(3)?,
                        start_line: row.get(4)?,
                        end_line: row.get(5)?,
                        body: row.get(6)?,
                        indexed_at: row.get(7)?,
                        name_path: row.get(8)?,
                        body_hash: row.get(9)?,
                        duplicate_index: row.get(10)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Get a symbol record by name (exact match).
    pub fn get_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>, Fts5StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, kind, start_line, end_line, body, indexed_at,
                    name_path, body_hash, duplicate_index
             FROM fts5_symbols WHERE name = ?1 ORDER BY file_id, duplicate_index",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(SymbolRecord {
                id: row.get(0)?,
                file_id: row.get(1)?,
                name: row.get(2)?,
                kind: row.get(3)?,
                start_line: row.get(4)?,
                end_line: row.get(5)?,
                body: row.get(6)?,
                indexed_at: row.get(7)?,
                name_path: row.get(8)?,
                body_hash: row.get(9)?,
                duplicate_index: row.get(10)?,
            })
        })?;
        let symbols = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(symbols)
    }

    /// Count symbols in the index.
    pub fn symbol_count(&self) -> Result<usize, Fts5StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fts5_symbols", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    // -----------------------------------------------------------------------
    // Chunk operations
    // -----------------------------------------------------------------------

    /// Upsert a chunk record. Returns the chunk ID.
    pub fn upsert_chunk(
        &self,
        file_id: i64,
        start_line: u32,
        end_line: u32,
        chunk_kind: &str,
        symbol_name: &str,
        symbol_kind: &str,
        content_hash: &str,
        body: &str,
    ) -> Result<i64, Fts5StoreError> {
        let now = now_secs();
        let truncated_body = truncate_body(body, DEFAULT_MAX_BODY_CHARS, DEFAULT_MAX_BODY_LINES);

        // Check if a chunk with this file_id and line range already exists
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM fts5_chunks WHERE file_id = ?1 AND start_line = ?2 AND end_line = ?3",
                params![file_id, start_line, end_line],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            self.conn.execute(
                "UPDATE fts5_chunks SET chunk_kind = ?1, symbol_name = ?2, symbol_kind = ?3,
                 content_hash = ?4, body = ?5, indexed_at = ?6 WHERE id = ?7",
                params![
                    chunk_kind,
                    symbol_name,
                    symbol_kind,
                    content_hash,
                    truncated_body,
                    now,
                    id
                ],
            )?;
            Ok(id)
        } else {
            self.conn.execute(
                "INSERT INTO fts5_chunks (file_id, start_line, end_line, chunk_kind, symbol_name, symbol_kind, content_hash, body, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![file_id, start_line, end_line, chunk_kind, symbol_name, symbol_kind, content_hash, truncated_body, now],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    /// Get all chunks for a file.
    pub fn get_chunks_for_file(&self, file_id: i64) -> Result<Vec<ChunkRecord>, Fts5StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, start_line, end_line, chunk_kind, symbol_name, symbol_kind,
                    content_hash, body, indexed_at
             FROM fts5_chunks WHERE file_id = ?1 ORDER BY start_line",
        )?;
        let rows = stmt.query_map(params![file_id], |row| {
            Ok(ChunkRecord {
                id: row.get(0)?,
                file_id: row.get(1)?,
                start_line: row.get(2)?,
                end_line: row.get(3)?,
                chunk_kind: row.get(4)?,
                symbol_name: row.get(5)?,
                symbol_kind: row.get(6)?,
                content_hash: row.get(7)?,
                body: row.get(8)?,
                indexed_at: row.get(9)?,
            })
        })?;
        let chunks = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(chunks)
    }

    /// Get a chunk by ID.
    pub fn get_chunk(&self, chunk_id: i64) -> Result<Option<ChunkRecord>, Fts5StoreError> {
        let result = self
            .conn
            .query_row(
                "SELECT id, file_id, start_line, end_line, chunk_kind, symbol_name, symbol_kind,
                        content_hash, body, indexed_at
                 FROM fts5_chunks WHERE id = ?1",
                params![chunk_id],
                |row| {
                    Ok(ChunkRecord {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        start_line: row.get(2)?,
                        end_line: row.get(3)?,
                        chunk_kind: row.get(4)?,
                        symbol_name: row.get(5)?,
                        symbol_kind: row.get(6)?,
                        content_hash: row.get(7)?,
                        body: row.get(8)?,
                        indexed_at: row.get(9)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Delete all chunks for a file.
    pub fn delete_chunks_for_file(&self, file_id: i64) -> Result<(), Fts5StoreError> {
        self.conn.execute(
            "DELETE FROM fts5_chunks WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(())
    }

    /// Count chunks in the index.
    pub fn chunk_count(&self) -> Result<usize, Fts5StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM fts5_chunks", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    // -----------------------------------------------------------------------
    // Transaction support
    // -----------------------------------------------------------------------

    /// Begin a transaction for batch operations.
    pub fn begin_transaction(&mut self) -> Result<Transaction<'_>, Fts5StoreError> {
        Ok(self.conn.transaction()?)
    }

    // -----------------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------------

    /// Get FTS5 row counts for diagnostics.
    pub fn fts_row_counts(&self) -> Result<FtsRowCounts, Fts5StoreError> {
        let symbols_fts: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM fts5_symbols_fts", [], |row| {
                    row.get(0)
                })?;
        let bodies_fts: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM fts5_symbol_bodies_fts", [], |row| {
                    row.get(0)
                })?;
        let paths_fts: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM fts5_paths_fts", [], |row| row.get(0))?;
        Ok(FtsRowCounts {
            symbols_fts: symbols_fts as usize,
            bodies_fts: bodies_fts as usize,
            paths_fts: paths_fts as usize,
        })
    }

    /// Run integrity check on the database.
    pub fn integrity_check(&self) -> Result<String, Fts5StoreError> {
        let result: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// FtsRowCounts
// ---------------------------------------------------------------------------

/// Row counts for FTS5 virtual tables (for diagnostics).
#[derive(Debug, Clone)]
pub struct FtsRowCounts {
    pub symbols_fts: usize,
    pub bodies_fts: usize,
    pub paths_fts: usize,
}

/// Information about a stale file in the index.
#[derive(Debug, Clone)]
pub struct StaleFileInfo {
    /// Relative path of the file.
    pub path: String,
    /// When the file was last indexed (seconds since epoch).
    pub indexed_mtime: i64,
    /// Current mtime on disk (-1 if file was deleted).
    pub current_mtime: i64,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get current time as seconds since UNIX epoch.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Truncate a symbol body to fit within character and line limits.
fn truncate_body(body: &str, max_chars: usize, max_lines: usize) -> String {
    let mut result = String::new();
    let mut char_count = 0;

    for (line_idx, line) in body.lines().enumerate() {
        // Check limits BEFORE adding this line
        if line_idx >= max_lines {
            break;
        }
        if char_count + line.len() > max_chars {
            let remaining = max_chars.saturating_sub(char_count);
            if remaining > 0 {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&line[..remaining.min(line.len())]);
            }
            break;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(line);
        char_count += line.len();
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_in_memory_store_succeeds() {
        let store = Fts5Store::open_in_memory().expect("failed to create in-memory store");
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn upsert_and_get_file() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc123", 1000, 512, 1)
            .unwrap();
        assert!(file_id > 0);

        let file = store.get_file_by_path("src/main.rs").unwrap();
        assert!(file.is_some());
        let file = file.unwrap();
        assert_eq!(file.path, "src/main.rs");
        assert_eq!(file.hash, "abc123");
        assert_eq!(file.mtime_secs, 1000);
        assert_eq!(file.size_bytes, 512);
        assert_eq!(file.generation, 1);
    }

    #[test]
    fn upsert_file_updates_on_conflict() {
        let store = Fts5Store::open_in_memory().unwrap();

        let id1 = store
            .upsert_file("src/main.rs", "abc", 1000, 100, 1)
            .unwrap();
        let id2 = store
            .upsert_file("src/main.rs", "def", 2000, 200, 2)
            .unwrap();
        assert_eq!(id1, id2); // Same rowid on upsert

        let file = store.get_file_by_path("src/main.rs").unwrap().unwrap();
        assert_eq!(file.hash, "def");
        assert_eq!(file.mtime_secs, 2000);
        assert_eq!(file.size_bytes, 200);
        assert_eq!(file.generation, 2);
    }

    #[test]
    fn upsert_and_get_symbol() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc", 1000, 512, 1)
            .unwrap();
        let sym_id = store
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}", "", "")
            .unwrap();
        assert!(sym_id > 0);

        let sym = store.get_symbol(sym_id).unwrap().unwrap();
        assert_eq!(sym.name, "main");
        assert_eq!(sym.kind, "function");
        assert_eq!(sym.body, "fn main() {}");
    }

    #[test]
    fn symbol_name_exact_lookup() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_symbol(file_id, "Foo", "struct", 1, 5, "struct Foo {}", "", "")
            .unwrap();
        store
            .upsert_symbol(file_id, "Bar", "struct", 6, 10, "struct Bar {}", "", "")
            .unwrap();

        let results = store.get_symbol_by_name("Foo").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Foo");
    }

    #[test]
    fn delete_file_cascades_to_symbols() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}", "", "")
            .unwrap();

        assert_eq!(store.symbol_count().unwrap(), 1);

        store.delete_file(file_id).unwrap();

        assert_eq!(store.file_count().unwrap(), 0);
        assert_eq!(store.symbol_count().unwrap(), 0);
    }

    #[test]
    fn fts_row_counts_match() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}", "", "")
            .unwrap();

        let counts = store.fts_row_counts().unwrap();
        assert_eq!(counts.symbols_fts, 1);
        assert_eq!(counts.bodies_fts, 1);
        assert_eq!(counts.paths_fts, 1);
    }

    #[test]
    fn integrity_check_passes() {
        let store = Fts5Store::open_in_memory().unwrap();
        let result = store.integrity_check().unwrap();
        assert_eq!(result, "ok");
    }

    #[test]
    fn truncate_body_respects_limits() {
        let body = "line1\nline2\nline3\nline4\nline5";
        let truncated = truncate_body(body, 20, 3);
        assert!(truncated.lines().count() <= 3);
        assert!(truncated.len() <= 20 + 20); // Allow some overhead for truncation message
    }

    #[test]
    fn rebuild_resets_schema() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/main.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}", "", "")
            .unwrap();

        assert_eq!(store.file_count().unwrap(), 1);

        store.rebuild().unwrap();

        assert_eq!(store.file_count().unwrap(), 0);
        assert_eq!(store.symbol_count().unwrap(), 0);
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn upsert_file_returns_correct_id_on_conflict_update() {
        let store = Fts5Store::open_in_memory().unwrap();

        // Insert a file
        let id1 = store
            .upsert_file("src/main.rs", "hash1", 1000, 100, 1)
            .unwrap();
        assert!(id1 > 0);

        // Upsert with same path but different hash (conflict update)
        let id2 = store
            .upsert_file("src/main.rs", "hash2", 2000, 200, 2)
            .unwrap();

        // The returned ID must match the original row ID
        assert_eq!(
            id1, id2,
            "upsert_file must return the same ID on conflict update"
        );

        // Verify the file is correctly updated
        let file = store.get_file_by_path("src/main.rs").unwrap().unwrap();
        assert_eq!(file.id, id1);
        assert_eq!(file.hash, "hash2");
        assert_eq!(file.mtime_secs, 2000);
        assert_eq!(file.size_bytes, 200);
        assert_eq!(file.generation, 2);
    }

    #[test]
    fn exact_symbol_lookup_returns_correct_file_path() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/lib.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_symbol(
                file_id,
                "MyStruct",
                "struct",
                10,
                20,
                "struct MyStruct {}",
                "",
                "",
            )
            .unwrap();

        // Exact symbol lookup by name
        let results = store.get_symbol_by_name("MyStruct").unwrap();
        assert_eq!(results.len(), 1);

        // The symbol's file_id must reference the correct file
        let sym = &results[0];
        assert_eq!(sym.file_id, file_id);

        // Loading the file by ID must return the correct path
        let file = store.get_file_by_id(sym.file_id).unwrap().unwrap();
        assert_eq!(file.path, "src/lib.rs");
    }

    #[test]
    fn stale_files_detects_content_change() {
        let tmp = std::env::temp_dir().join("fts5_store_stale_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("test.rs");
        std::fs::write(&file_path, "fn test() {}\n").unwrap();

        let db_path = tmp.join("test.sqlite");
        let store = Fts5Store::open(&db_path).unwrap();

        // Index with a hash that doesn't match the actual file content
        store
            .upsert_file("test.rs", "wrong_hash", 1000, 100, 1)
            .unwrap();

        // stale_files should detect the content mismatch via hash comparison
        let stale = store.stale_files(&tmp).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, "test.rs");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stale_files_detects_size_change() {
        let tmp = std::env::temp_dir().join("fts5_store_stale_size_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let file_path = tmp.join("test.rs");
        std::fs::write(&file_path, "fn test() {}\n").unwrap();

        let db_path = tmp.join("test.sqlite");
        let store = Fts5Store::open(&db_path).unwrap();

        // Index with wrong size — stale_files should detect via size mismatch
        store.upsert_file("test.rs", "abc", 1000, 999, 1).unwrap();

        let stale = store.stale_files(&tmp).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].path, "test.rs");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generation_increments_on_update() {
        let store = Fts5Store::open_in_memory().unwrap();

        let id1 = store
            .upsert_file("src/main.rs", "hash1", 1000, 100, 1)
            .unwrap();
        let id2 = store
            .upsert_file("src/main.rs", "hash2", 2000, 200, 2)
            .unwrap();
        assert_eq!(id1, id2);

        let file = store.get_file_by_path("src/main.rs").unwrap().unwrap();
        assert_eq!(file.generation, 2);
    }

    // -----------------------------------------------------------------------
    // Chunk tests
    // -----------------------------------------------------------------------

    #[test]
    fn upsert_and_get_chunk() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("docs/readme.md", "abc", 1000, 512, 1)
            .unwrap();
        let chunk_id = store
            .upsert_chunk(
                file_id,
                1,
                10,
                chunk_kind::PARAGRAPH,
                "",
                "",
                "hash123",
                "Hello world",
            )
            .unwrap();
        assert!(chunk_id > 0);

        let chunk = store.get_chunk(chunk_id).unwrap().unwrap();
        assert_eq!(chunk.file_id, file_id);
        assert_eq!(chunk.start_line, 1);
        assert_eq!(chunk.end_line, 10);
        assert_eq!(chunk.chunk_kind, chunk_kind::PARAGRAPH);
        assert_eq!(chunk.content_hash, "hash123");
        assert_eq!(chunk.body, "Hello world");
    }

    #[test]
    fn chunk_upsert_updates_on_conflict() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("docs/readme.md", "abc", 1000, 512, 1)
            .unwrap();
        let id1 = store
            .upsert_chunk(file_id, 1, 10, chunk_kind::PARAGRAPH, "", "", "h1", "body1")
            .unwrap();
        let id2 = store
            .upsert_chunk(file_id, 1, 10, chunk_kind::HEADING, "", "", "h2", "body2")
            .unwrap();
        assert_eq!(id1, id2); // Same line range = same chunk

        let chunk = store.get_chunk(id1).unwrap().unwrap();
        assert_eq!(chunk.chunk_kind, chunk_kind::HEADING);
        assert_eq!(chunk.body, "body2");
    }

    #[test]
    fn get_chunks_for_file() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("docs/readme.md", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_chunk(file_id, 1, 10, chunk_kind::HEADING, "", "", "h1", "body1")
            .unwrap();
        store
            .upsert_chunk(
                file_id,
                12,
                25,
                chunk_kind::PARAGRAPH,
                "",
                "",
                "h2",
                "body2",
            )
            .unwrap();

        let chunks = store.get_chunks_for_file(file_id).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[1].start_line, 12);
    }

    #[test]
    fn chunk_count() {
        let store = Fts5Store::open_in_memory().unwrap();
        assert_eq!(store.chunk_count().unwrap(), 0);

        let file_id = store.upsert_file("test.md", "abc", 1000, 512, 1).unwrap();
        store
            .upsert_chunk(file_id, 1, 5, chunk_kind::PARAGRAPH, "", "", "h1", "body")
            .unwrap();
        assert_eq!(store.chunk_count().unwrap(), 1);
    }

    #[test]
    fn delete_file_cascades_to_chunks() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("docs/readme.md", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_chunk(file_id, 1, 10, chunk_kind::PARAGRAPH, "", "", "h1", "body")
            .unwrap();

        assert_eq!(store.chunk_count().unwrap(), 1);

        store.delete_file(file_id).unwrap();

        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    #[test]
    fn chunk_symbol_metadata() {
        let store = Fts5Store::open_in_memory().unwrap();

        let file_id = store
            .upsert_file("src/lib.rs", "abc", 1000, 512, 1)
            .unwrap();
        store
            .upsert_chunk(
                file_id,
                5,
                15,
                chunk_kind::SYMBOL,
                "my_function",
                "function",
                "hash123",
                "fn my_function() {}",
            )
            .unwrap();

        let chunks = store.get_chunks_for_file(file_id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_name, "my_function");
        assert_eq!(chunks[0].symbol_kind, "function");
    }
}
