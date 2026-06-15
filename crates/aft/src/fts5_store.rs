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
pub const SCHEMA_VERSION: i64 = 2;

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

            -- Symbol tracking table
            CREATE TABLE IF NOT EXISTS fts5_symbols (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id     INTEGER NOT NULL REFERENCES fts5_files(id) ON DELETE CASCADE,
                name        TEXT NOT NULL,
                kind        TEXT NOT NULL,
                start_line  INTEGER NOT NULL,
                end_line    INTEGER NOT NULL,
                body        TEXT NOT NULL,
                indexed_at  INTEGER NOT NULL
            );

            -- Indexes for exact/prefix symbol lookup
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON fts5_symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON fts5_symbols(file_id);
            CREATE INDEX IF NOT EXISTS idx_symbols_kind ON fts5_symbols(kind);

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

    /// Delete a file and all its symbols (cascade).
    pub fn delete_file(&self, file_id: i64) -> Result<(), Fts5StoreError> {
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
                 body = ?4, indexed_at = ?5 WHERE id = ?6",
                params![kind, start_line, end_line, truncated_body, now, id],
            )?;
            Ok(id)
        } else {
            self.conn.execute(
                "INSERT INTO fts5_symbols (file_id, name, kind, start_line, end_line, body, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![file_id, name, kind, start_line, end_line, truncated_body, now],
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
                "SELECT id, file_id, name, kind, start_line, end_line, body, indexed_at
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
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    /// Get a symbol record by name (exact match).
    pub fn get_symbol_by_name(&self, name: &str) -> Result<Vec<SymbolRecord>, Fts5StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, kind, start_line, end_line, body, indexed_at
             FROM fts5_symbols WHERE name = ?1 ORDER BY file_id",
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
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}")
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
            .upsert_symbol(file_id, "Foo", "struct", 1, 5, "struct Foo {}")
            .unwrap();
        store
            .upsert_symbol(file_id, "Bar", "struct", 6, 10, "struct Bar {}")
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
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}")
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
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}")
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
            .upsert_symbol(file_id, "main", "function", 1, 10, "fn main() {}")
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
            .upsert_symbol(file_id, "MyStruct", "struct", 10, 20, "struct MyStruct {}")
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
}
