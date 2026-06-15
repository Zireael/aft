//! Repository Intelligence Layer (RIL) graph indexer.
//!
//! Indexes files, symbols, imports, and reverse importers into the RIL database.
//! Supports incremental updates for changed files.

use rusqlite::{params, Connection};
use std::path::Path;

/// Graph indexer for the Repository Intelligence Layer.
pub struct RilIndexer {
    conn: Connection,
}

impl RilIndexer {
    /// Create a new indexer with the given database connection.
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Index a file into the RIL database.
    ///
    /// This function:
    /// 1. Inserts/updates the file record
    /// 2. Extracts symbols from the file
    /// 3. Extracts imports from the file
    /// 4. Updates reverse importer edges
    pub fn index_file(
        &mut self,
        file_path: &Path,
        content: &str,
        content_hash: &str,
        project_root: &Path,
    ) -> Result<IndexResult, IndexError> {
        let relative_path = file_path
            .strip_prefix(project_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let language = detect_language(&relative_path);
        let size_bytes = content.len() as i64;
        let mtime_secs = std::fs::metadata(file_path)
            .map(|m| {
                m.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        // Insert/update file record
        let file_id = self.upsert_file(
            &relative_path,
            content_hash,
            &language,
            size_bytes,
            mtime_secs,
        )?;

        // Extract and index symbols
        let symbols = extract_symbols(content, &language);
        let symbol_count = self.index_symbols(file_id, &symbols)?;

        // Extract and index imports
        let imports = extract_imports(content, &language);
        let import_count = self.index_imports(file_id, &imports, project_root)?;

        // Update reverse importer edges
        let reverse_count = self.update_reverse_importers(file_id, &imports, project_root)?;

        Ok(IndexResult {
            file_id,
            symbol_count,
            import_count,
            reverse_count,
        })
    }

    /// Upsert a file record and return its ID.
    fn upsert_file(
        &mut self,
        path: &str,
        content_hash: &str,
        language: &str,
        size_bytes: i64,
        mtime_secs: i64,
    ) -> Result<i64, IndexError> {
        // Try to get existing file ID
        let existing_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM ril_files WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .ok();

        if let Some(id) = existing_id {
            // Update existing file
            self.conn.execute(
                "UPDATE ril_files SET content_hash = ?1, language = ?2, size_bytes = ?3, 
                 mtime_secs = ?4, generation = generation + 1, indexed_at = ?5
                 WHERE id = ?6",
                params![
                    content_hash,
                    language,
                    size_bytes,
                    mtime_secs,
                    now_secs(),
                    id
                ],
            )?;
            Ok(id)
        } else {
            // Insert new file
            self.conn.execute(
                "INSERT INTO ril_files (path, content_hash, language, size_bytes, mtime_secs, generation, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
                params![path, content_hash, language, size_bytes, mtime_secs, now_secs()],
            )?;
            Ok(self.conn.last_insert_rowid())
        }
    }

    /// Index symbols for a file.
    fn index_symbols(&mut self, file_id: i64, symbols: &[Symbol]) -> Result<usize, IndexError> {
        // Delete existing symbols for this file
        self.conn.execute(
            "DELETE FROM ril_symbols WHERE file_id = ?1",
            params![file_id],
        )?;

        let mut count = 0;
        for symbol in symbols {
            self.conn.execute(
                "INSERT INTO ril_symbols (file_id, name, kind, start_line, end_line, body_hash, name_path, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
                params![
                    file_id,
                    symbol.name,
                    symbol.kind,
                    symbol.start_line,
                    symbol.end_line,
                    symbol.body_hash,
                    symbol.name_path,
                ],
            )?;
            count += 1;
        }

        Ok(count)
    }

    /// Index imports for a file.
    fn index_imports(
        &mut self,
        file_id: i64,
        imports: &[Import],
        project_root: &Path,
    ) -> Result<usize, IndexError> {
        // Delete existing import edges for this file
        self.conn.execute(
            "DELETE FROM ril_edges WHERE source_id = ?1 AND source_type = 'file' AND edge_type = 'import'",
            params![file_id],
        )?;

        let mut count = 0;
        for import in imports {
            // Try to resolve the import target
            let target_id = self.resolve_import_target(import, project_root);

            self.conn.execute(
                "INSERT INTO ril_edges (source_id, source_type, target_id, target_type, edge_type, metadata, created_at)
                 VALUES (?1, 'file', ?2, ?3, 'import', ?4, ?5)",
                params![
                    file_id,
                    target_id.unwrap_or(-1), // -1 for unresolved
                    if target_id.is_some() { "file" } else { "unresolved" },
                    serde_json::to_string(&import.metadata).unwrap_or_default(),
                    now_secs(),
                ],
            )?;
            count += 1;
        }

        Ok(count)
    }

    /// Update reverse importer edges.
    fn update_reverse_importers(
        &mut self,
        file_id: i64,
        _imports: &[Import],
        _project_root: &Path,
    ) -> Result<usize, IndexError> {
        // For each import in this file, check if any other file imports this file
        // and create reverse edges
        let mut count = 0;

        // Get all files that import this file
        let importing_files: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT e.source_id, f.path 
                 FROM ril_edges e 
                 JOIN ril_files f ON e.target_id = f.id 
                 WHERE e.target_id = ?1 AND e.edge_type = 'import' AND e.source_type = 'file'",
            )?;

            let results: Vec<(i64, String)> = stmt
                .query_map(params![file_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            results
        };

        // For each importing file, create a reverse edge
        for (importer_id, _importer_path) in importing_files {
            // Check if reverse edge already exists
            let exists: bool = self.conn.query_row(
                "SELECT COUNT(*) > 0 FROM ril_edges 
                 WHERE source_id = ?1 AND target_id = ?2 AND edge_type = 'imported_by'",
                params![importer_id, file_id],
                |row| row.get(0),
            )?;

            if !exists {
                self.conn.execute(
                    "INSERT INTO ril_edges (source_id, source_type, target_id, target_type, edge_type, created_at)
                     VALUES (?1, 'file', ?2, 'file', 'imported_by', ?3)",
                    params![importer_id, file_id, now_secs()],
                )?;
                count += 1;
            }
        }

        Ok(count)
    }

    /// Resolve an import to a file ID.
    fn resolve_import_target(&self, import: &Import, project_root: &Path) -> Option<i64> {
        // Simple resolution: check if the import path matches a file in the database
        let possible_paths = generate_possible_paths(&import.path, project_root);

        for path in &possible_paths {
            if let Ok(id) = self.conn.query_row(
                "SELECT id FROM ril_files WHERE path = ?1",
                params![path],
                |row| row.get(0),
            ) {
                return Some(id);
            }
        }

        None
    }

    /// Get all imports for a file.
    pub fn get_imports(&self, file_id: i64) -> Result<Vec<ImportInfo>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.target_id, f.path, e.metadata 
             FROM ril_edges e 
             LEFT JOIN ril_files f ON e.target_id = f.id 
             WHERE e.source_id = ?1 AND e.edge_type = 'import' AND e.source_type = 'file'",
        )?;

        let imports = stmt
            .query_map(params![file_id], |row| {
                Ok(ImportInfo {
                    target_id: row.get(0)?,
                    target_path: row.get(1)?,
                    metadata: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(imports)
    }

    /// Get all files that import a given file (reverse importers).
    pub fn get_reverse_importers(&self, file_id: i64) -> Result<Vec<ImporterInfo>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.source_id, f.path 
             FROM ril_edges e 
             JOIN ril_files f ON e.source_id = f.id 
             WHERE e.target_id = ?1 AND e.edge_type = 'imported_by' AND e.source_type = 'file'",
        )?;

        let importers = stmt
            .query_map(params![file_id], |row| {
                Ok(ImporterInfo {
                    file_id: row.get(0)?,
                    file_path: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(importers)
    }

    /// Get all symbols for a file.
    pub fn get_symbols(&self, file_id: i64) -> Result<Vec<SymbolInfo>, IndexError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, start_line, end_line, body_hash, name_path 
             FROM ril_symbols WHERE file_id = ?1",
        )?;

        let symbols = stmt
            .query_map(params![file_id], |row| {
                Ok(SymbolInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    start_line: row.get(3)?,
                    end_line: row.get(4)?,
                    body_hash: row.get(5)?,
                    name_path: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(symbols)
    }

    /// Get index statistics.
    pub fn stats(&self) -> Result<IndexStats, IndexError> {
        let file_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM ril_files", [], |row| row.get(0))?;

        let symbol_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ril_symbols", [], |row| row.get(0))?;

        let edge_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM ril_edges", [], |row| row.get(0))?;

        let unresolved_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ril_edges WHERE target_type = 'unresolved'",
            [],
            |row| row.get(0),
        )?;

        Ok(IndexStats {
            file_count,
            symbol_count,
            edge_count,
            unresolved_count,
        })
    }
}

/// Result of indexing a file.
#[derive(Debug, Clone)]
pub struct IndexResult {
    pub file_id: i64,
    pub symbol_count: usize,
    pub import_count: usize,
    pub reverse_count: usize,
}

/// Error type for indexer operations.
#[derive(Debug)]
pub enum IndexError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Sqlite(e) => write!(f, "database error: {e}"),
            IndexError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Sqlite(e)
    }
}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        IndexError::Io(e)
    }
}

/// Extracted symbol information.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body_hash: String,
    pub name_path: String,
}

/// Extracted import information.
#[derive(Debug, Clone)]
pub struct Import {
    pub path: String,
    pub kind: ImportKind,
    pub metadata: std::collections::HashMap<String, String>,
}

/// Import kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    /// ES module import (import ... from '...')
    EsModule,
    /// CommonJS require (require('...'))
    CommonJs,
    /// Rust use statement (use crate::...)
    Rust,
    /// Python import (import ... / from ... import ...)
    Python,
    /// Go import (import "...")
    Go,
    /// Unknown
    Unknown,
}

/// Import information for querying.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    pub target_id: Option<i64>,
    pub target_path: Option<String>,
    pub metadata: String,
}

/// Importer information for querying.
#[derive(Debug, Clone)]
pub struct ImporterInfo {
    pub file_id: i64,
    pub file_path: String,
}

/// Symbol information for querying.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    pub body_hash: Option<String>,
    pub name_path: Option<String>,
}

/// Index statistics.
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub file_count: i64,
    pub symbol_count: i64,
    pub edge_count: i64,
    pub unresolved_count: i64,
}

/// Detect the language of a file based on its extension.
fn detect_language(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "ts" | "tsx" => "typescript".to_string(),
        "js" | "jsx" => "javascript".to_string(),
        "rs" => "rust".to_string(),
        "py" => "python".to_string(),
        "go" => "go".to_string(),
        "java" => "java".to_string(),
        "c" | "cpp" | "h" | "hpp" => "c".to_string(),
        "cs" => "csharp".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Extract symbols from source code.
fn extract_symbols(content: &str, language: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut in_comment = false;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Track block comments
        if trimmed.contains("/*") && !trimmed.contains("*/") {
            in_comment = true;
            continue;
        }
        if in_comment {
            if trimmed.contains("*/") {
                in_comment = false;
            }
            continue;
        }

        // Skip single-line comments
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }
        // Skip preprocessor directives and shebang
        if trimmed.starts_with('#') && !trimmed.starts_with("#[") {
            continue;
        }

        // Extract symbols based on language
        let symbol = match language {
            "rust" => extract_rust_symbol(trimmed, i as u32 + 1),
            "typescript" | "javascript" => extract_ts_symbol(trimmed, i as u32 + 1),
            "python" => extract_python_symbol(trimmed, i as u32 + 1),
            "go" => extract_go_symbol(trimmed, i as u32 + 1),
            _ => None,
        };

        if let Some(sym) = symbol {
            symbols.push(sym);
        }
    }

    symbols
}

/// Clean a raw symbol name by stripping parameter lists and trailing junk.
fn clean_symbol_name(raw: &str) -> String {
    // Strip everything from the first '(' onward (parameter list)
    let without_params = match raw.find('(') {
        Some(pos) => &raw[..pos],
        None => raw,
    };
    // Strip trailing non-alphanumeric/underscore characters (braces, colons, etc.)
    let mut name = without_params;
    while let Some(last) = name.chars().last() {
        if last.is_alphanumeric() || last == '_' {
            break;
        }
        name = &name[..name.len() - last.len_utf8()];
    }
    name.to_string()
}

/// Extract Rust symbol from a line.
fn extract_rust_symbol(line: &str, line_num: u32) -> Option<Symbol> {
    let (raw_name, kind) = if line.starts_with("pub fn ") || line.starts_with("fn ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "function")
    } else if line.starts_with("pub struct ") || line.starts_with("struct ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "struct")
    } else if line.starts_with("pub enum ") || line.starts_with("enum ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "enum")
    } else if line.starts_with("pub trait ") || line.starts_with("trait ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "trait")
    } else if line.starts_with("pub type ") || line.starts_with("type ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "type")
    } else if line.starts_with("pub mod ") || line.starts_with("mod ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("pub") { 2 } else { 1 })?;
        (raw, "module")
    } else {
        return None;
    };

    Some(Symbol {
        name: clean_symbol_name(raw_name),
        kind: kind.to_string(),
        start_line: line_num,
        end_line: line_num,
        body_hash: String::new(),
        name_path: String::new(),
    })
}

/// Extract TypeScript/JavaScript symbol from a line.
fn extract_ts_symbol(line: &str, line_num: u32) -> Option<Symbol> {
    let (raw_name, kind) = if line.starts_with("export function ") || line.starts_with("function ")
    {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("export") { 2 } else { 1 })?;
        (raw, "function")
    } else if line.starts_with("export class ") || line.starts_with("class ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("export") { 2 } else { 1 })?;
        (raw, "class")
    } else if line.starts_with("export interface ") || line.starts_with("interface ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("export") { 2 } else { 1 })?;
        (raw, "interface")
    } else if line.starts_with("export type ") || line.starts_with("type ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("export") { 2 } else { 1 })?;
        (raw, "type")
    } else if line.starts_with("export const ") || line.starts_with("const ") {
        let raw = line
            .split_whitespace()
            .nth(if line.starts_with("export") { 2 } else { 1 })?;
        (raw, "constant")
    } else {
        return None;
    };

    Some(Symbol {
        name: clean_symbol_name(raw_name),
        kind: kind.to_string(),
        start_line: line_num,
        end_line: line_num,
        body_hash: String::new(),
        name_path: String::new(),
    })
}

/// Extract Python symbol from a line.
fn extract_python_symbol(line: &str, line_num: u32) -> Option<Symbol> {
    let (raw_name, kind) = if line.starts_with("def ") {
        let raw = line.split_whitespace().nth(1)?;
        (raw, "function")
    } else if line.starts_with("class ") {
        let raw = line.split_whitespace().nth(1)?;
        (raw, "class")
    } else {
        return None;
    };

    Some(Symbol {
        name: clean_symbol_name(raw_name),
        kind: kind.to_string(),
        start_line: line_num,
        end_line: line_num,
        body_hash: String::new(),
        name_path: String::new(),
    })
}

/// Extract Go symbol from a line.
fn extract_go_symbol(line: &str, line_num: u32) -> Option<Symbol> {
    let (raw_name, kind) = if line.starts_with("func ") {
        let raw = line.split_whitespace().nth(1)?;
        (raw, "function")
    } else if line.starts_with("type ") {
        let raw = line.split_whitespace().nth(1)?;
        (raw, "type")
    } else if line.starts_with("var ") {
        let raw = line.split_whitespace().nth(1)?;
        (raw, "variable")
    } else {
        return None;
    };

    Some(Symbol {
        name: clean_symbol_name(raw_name),
        kind: kind.to_string(),
        start_line: line_num,
        end_line: line_num,
        body_hash: String::new(),
        name_path: String::new(),
    })
}

/// Extract imports from source code.
fn extract_imports(content: &str, language: &str) -> Vec<Import> {
    let mut imports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        let import = match language {
            "rust" => extract_rust_import(trimmed),
            "typescript" | "javascript" => extract_ts_import(trimmed),
            "python" => extract_python_import(trimmed),
            "go" => extract_go_import(trimmed),
            _ => None,
        };

        if let Some(imp) = import {
            imports.push(imp);
        }
    }

    imports
}

/// Extract Rust import from a line.
fn extract_rust_import(line: &str) -> Option<Import> {
    if !line.starts_with("use ") || !line.contains(';') {
        return None;
    }

    let path = line.trim_start_matches("use ").trim_end_matches(';').trim();

    Some(Import {
        path: path.to_string(),
        kind: ImportKind::Rust,
        metadata: std::collections::HashMap::new(),
    })
}

/// Extract TypeScript/JavaScript import from a line.
fn extract_ts_import(line: &str) -> Option<Import> {
    if !line.starts_with("import ") {
        return None;
    }

    // Extract the module path (after 'from')
    if let Some(from_pos) = line.find(" from ") {
        let path = line[from_pos + 6..]
            .trim()
            .trim_end_matches(';')
            .trim_matches('\'')
            .trim_matches('"');
        return Some(Import {
            path: path.to_string(),
            kind: ImportKind::EsModule,
            metadata: std::collections::HashMap::new(),
        });
    }

    // CommonJS require
    if line.contains("require(") {
        if let Some(start) = line.find("require('") {
            let rest = &line[start + 9..];
            if let Some(end) = rest.find('\'') {
                return Some(Import {
                    path: rest[..end].to_string(),
                    kind: ImportKind::CommonJs,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
    }

    None
}

/// Extract Python import from a line.
fn extract_python_import(line: &str) -> Option<Import> {
    if line.starts_with("import ") {
        let raw = line.trim_start_matches("import ").trim();
        // Handle `import X as Y` — take only the module name
        let path = raw.split_whitespace().next()?;
        Some(Import {
            path: path.to_string(),
            kind: ImportKind::Python,
            metadata: std::collections::HashMap::new(),
        })
    } else if line.starts_with("from ") && line.contains(" import ") {
        let module = line
            .trim_start_matches("from ")
            .split_once(" import ")
            .map(|(m, _)| m.trim())?;
        Some(Import {
            path: module.to_string(),
            kind: ImportKind::Python,
            metadata: std::collections::HashMap::new(),
        })
    } else {
        None
    }
}

/// Extract Go import from a line.
///
/// Handles both `import "fmt"` (statement) and bare `"fmt"` (inside import block).
fn extract_go_import(line: &str) -> Option<Import> {
    let trimmed = line.trim();

    // Handle `import "path"` statement form
    if let Some(rest) = trimmed.strip_prefix("import ") {
        let path = rest.trim().trim_matches('"');
        if !path.is_empty() {
            return Some(Import {
                path: path.to_string(),
                kind: ImportKind::Go,
                metadata: std::collections::HashMap::new(),
            });
        }
        return None;
    }

    // Handle bare `"path"` inside an import block
    if trimmed.starts_with('"') && trimmed.ends_with('"') {
        let path = trimmed.trim_matches('"');
        if !path.is_empty() {
            return Some(Import {
                path: path.to_string(),
                kind: ImportKind::Go,
                metadata: std::collections::HashMap::new(),
            });
        }
    }

    None
}

/// Generate possible file paths for an import.
fn generate_possible_paths(import_path: &str, _project_root: &Path) -> Vec<String> {
    let mut paths = Vec::new();

    // Direct path
    paths.push(import_path.to_string());

    // With .rs extension
    if !import_path.ends_with(".rs") {
        paths.push(format!("{import_path}.rs"));
    }

    // With /mod.rs
    if !import_path.ends_with(".rs") {
        paths.push(format!("{import_path}/mod.rs"));
    }

    // With .ts extension
    if !import_path.ends_with(".ts") && !import_path.ends_with(".tsx") {
        paths.push(format!("{import_path}.ts"));
        paths.push(format!("{import_path}.tsx"));
    }

    // With index.ts
    if !import_path.ends_with(".ts") {
        paths.push(format!("{import_path}/index.ts"));
        paths.push(format!("{import_path}/index.tsx"));
    }

    // With .py extension
    if !import_path.ends_with(".py") {
        paths.push(format!("{import_path}.py"));
        paths.push(format!("{import_path}/__init__.py"));
    }

    // With .go extension
    if !import_path.ends_with(".go") {
        paths.push(format!("{import_path}.go"));
    }

    paths
}

/// Get current time in seconds since UNIX epoch.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_rs() {
        assert_eq!(detect_language("src/main.rs"), "rust");
        assert_eq!(detect_language("lib.rs"), "rust");
    }

    #[test]
    fn detect_language_ts() {
        assert_eq!(detect_language("src/app.ts"), "typescript");
        assert_eq!(detect_language("src/app.tsx"), "typescript");
    }

    #[test]
    fn detect_language_js() {
        assert_eq!(detect_language("src/app.js"), "javascript");
        assert_eq!(detect_language("src/app.jsx"), "javascript");
    }

    #[test]
    fn detect_language_py() {
        assert_eq!(detect_language("src/app.py"), "python");
    }

    #[test]
    fn detect_language_go() {
        assert_eq!(detect_language("src/app.go"), "go");
    }

    #[test]
    fn extract_rust_symbols() {
        let content = r#"
pub fn main() {
    println!("Hello, world!");
}

struct Foo {
    x: i32,
}

impl Foo {
    fn bar(&self) {}
}
"#;
        let symbols = extract_symbols(content, "rust");
        assert!(symbols.len() >= 3);
        assert!(symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == "function"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == "struct"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "bar" && s.kind == "function"));
    }

    #[test]
    fn extract_ts_symbols() {
        let content = r#"
export function main() {
    console.log("Hello, world!");
}

class Foo {
    x: number;
}

interface Bar {
    y: string;
}
"#;
        let symbols = extract_symbols(content, "typescript");
        assert!(symbols.len() >= 3);
        assert!(symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == "function"));
        assert!(symbols.iter().any(|s| s.name == "Foo" && s.kind == "class"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Bar" && s.kind == "interface"));
    }

    #[test]
    fn extract_python_symbols() {
        let content = r#"
def main():
    print("Hello, world!")

class Foo:
    pass
"#;
        let symbols = extract_symbols(content, "python");
        assert!(symbols.len() >= 2);
        assert!(symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == "function"));
        assert!(symbols.iter().any(|s| s.name == "Foo" && s.kind == "class"));
    }

    #[test]
    fn extract_go_symbols() {
        let content = r#"
func main() {
    fmt.Println("Hello, world!")
}

type Foo struct {
    X int
}
"#;
        let symbols = extract_symbols(content, "go");
        assert!(symbols.len() >= 2);
        assert!(symbols
            .iter()
            .any(|s| s.name == "main" && s.kind == "function"));
        assert!(symbols.iter().any(|s| s.name == "Foo" && s.kind == "type"));
    }

    #[test]
    fn extract_rust_imports() {
        let content = r#"
use std::io;
use crate::module::Foo;
use self::bar::Baz;
"#;
        let imports = extract_imports(content, "rust");
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].path, "std::io");
        assert_eq!(imports[1].path, "crate::module::Foo");
        assert_eq!(imports[2].path, "self::bar::Baz");
    }

    #[test]
    fn extract_ts_imports() {
        let content = r#"
import { foo } from 'bar';
import { baz } from './baz';
import require from 'require';
"#;
        let imports = extract_imports(content, "typescript");
        assert!(imports.len() >= 2);
        assert_eq!(imports[0].path, "bar");
        assert_eq!(imports[1].path, "./baz");
    }

    #[test]
    fn extract_python_imports() {
        let content = r#"
import os
from pathlib import Path
import sys as system
"#;
        let imports = extract_imports(content, "python");
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].path, "os");
        assert_eq!(imports[1].path, "pathlib");
        assert_eq!(imports[2].path, "sys");
    }

    #[test]
    fn extract_go_imports() {
        let content = r#"
import "fmt"
import "os"
import "github.com/foo/bar"
"#;
        let imports = extract_imports(content, "go");
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].path, "fmt");
        assert_eq!(imports[1].path, "os");
        assert_eq!(imports[2].path, "github.com/foo/bar");
    }

    #[test]
    fn generate_possible_paths_test() {
        let paths = generate_possible_paths("src/main", Path::new("."));
        assert!(paths.contains(&"src/main".to_string()));
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"src/main.ts".to_string()));
        assert!(paths.contains(&"src/main.py".to_string()));
        assert!(paths.contains(&"src/main.go".to_string()));
    }
}
