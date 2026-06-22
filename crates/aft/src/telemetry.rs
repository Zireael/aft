//! Telemetry for retrieval intelligence queries.
//!
//! Persists retrieval run data to SQLite for analysis and debugging.
//! Schema: retrieval_runs, candidate_scores, fusion_scores tables.
//! Security: query_raw is NULL by default; no snippet text persisted.

use rusqlite::{params, Connection};

use crate::intelligence_config::TelemetryConfig;

/// Schema version for telemetry tables.
const TELEMETRY_SCHEMA_VERSION: u32 = 1;

/// Initialize telemetry tables in the database.
///
/// Creates retrieval_runs, candidate_scores, and fusion_scores tables
/// if they don't already exist.
pub fn init_telemetry_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS retrieval_runs (
            run_id TEXT PRIMARY KEY,
            query_hash TEXT NOT NULL,
            query_raw TEXT,
            query_kind TEXT,
            timestamp TEXT NOT NULL,
            latency_ms REAL,
            profile TEXT,
            backend_config TEXT,
            context_exhausted INTEGER,
            reranker_skipped_reason TEXT
        );

        CREATE TABLE IF NOT EXISTS candidate_scores (
            run_id TEXT NOT NULL,
            chunk_id TEXT,
            source_lane TEXT,
            raw_rank INTEGER,
            raw_score REAL,
            normalized_score REAL,
            is_exact_hit INTEGER,
            exact_hit_floor_applied INTEGER,
            FOREIGN KEY (run_id) REFERENCES retrieval_runs(run_id)
        );

        CREATE TABLE IF NOT EXISTS fusion_scores (
            run_id TEXT NOT NULL,
            chunk_id TEXT,
            rrf_score REAL,
            exact_hit_floor_applied INTEGER,
            final_score REAL,
            provenance_json TEXT,
            FOREIGN KEY (run_id) REFERENCES retrieval_runs(run_id)
        );

        CREATE TABLE IF NOT EXISTS telemetry_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .map_err(|e| format!("Failed to create telemetry schema: {e}"))?;

    // Record schema version
    conn.execute(
        "INSERT OR REPLACE INTO telemetry_meta (key, value) VALUES ('schema_version', ?1)",
        params![TELEMETRY_SCHEMA_VERSION.to_string()],
    )
    .map_err(|e| format!("Failed to record schema version: {e}"))?;

    Ok(())
}

/// Hash a query string with the salt for correlation.
///
/// Hash is for local correlation only, not anonymization.
pub fn hash_query(query: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(query.as_bytes());
    hasher.update(salt.as_bytes());
    let result = hasher.finalize();
    // Manual hex encoding to avoid `hex` crate dependency
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Generate a simple unique run ID (timestamp-based, no uuid crate needed).
fn generate_run_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("run-{ts:x}")
}

/// Get current UTC timestamp as RFC3339-like string.
fn now_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple ISO-like format without chrono dependency
    format!("{secs}")
}

/// Write a retrieval run to telemetry.
///
/// Returns the run_id.
pub fn write_retrieval_run(
    conn: &Connection,
    config: &TelemetryConfig,
    query: &str,
    query_kind: &str,
    latency_ms: f64,
    profile: &str,
    backend_config: &str,
    context_exhausted: bool,
    reranker_skipped_reason: Option<&str>,
) -> Result<String, String> {
    let run_id = generate_run_id();
    let timestamp = now_timestamp();

    // Determine query storage based on config
    let query_hash = hash_query(query, &config.telemetry_query_hash_salt);
    let query_raw = if config.telemetry_store_query == "raw" {
        Some(query.to_string())
    } else {
        None
    };

    conn.execute(
        "INSERT INTO retrieval_runs (
            run_id, query_hash, query_raw, query_kind, timestamp,
            latency_ms, profile, backend_config, context_exhausted, reranker_skipped_reason
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            run_id,
            query_hash,
            query_raw,
            query_kind,
            timestamp,
            latency_ms,
            profile,
            backend_config,
            context_exhausted as i32,
            reranker_skipped_reason,
        ],
    )
    .map_err(|e| format!("Failed to write retrieval run: {e}"))?;

    Ok(run_id)
}

/// Write candidate scores for a retrieval run.
pub fn write_candidate_scores(
    conn: &Connection,
    run_id: &str,
    candidates: &[CandidateScoreRow],
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "INSERT INTO candidate_scores (
                run_id, chunk_id, source_lane, raw_rank, raw_score,
                normalized_score, is_exact_hit, exact_hit_floor_applied
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .map_err(|e| format!("Failed to prepare candidate_scores insert: {e}"))?;

    for c in candidates {
        stmt.execute(params![
            run_id,
            c.chunk_id,
            c.source_lane,
            c.raw_rank,
            c.raw_score,
            c.normalized_score,
            c.is_exact_hit as i32,
            c.exact_hit_floor_applied as i32,
        ])
        .map_err(|e| format!("Failed to write candidate score: {e}"))?;
    }

    Ok(())
}

/// Write fusion scores for a retrieval run.
pub fn write_fusion_scores(
    conn: &Connection,
    run_id: &str,
    scores: &[FusionScoreRow],
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "INSERT INTO fusion_scores (
                run_id, chunk_id, rrf_score, exact_hit_floor_applied,
                final_score, provenance_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .map_err(|e| format!("Failed to prepare fusion_scores insert: {e}"))?;

    for s in scores {
        stmt.execute(params![
            run_id,
            s.chunk_id,
            s.rrf_score,
            s.exact_hit_floor_applied as i32,
            s.final_score,
            s.provenance_json,
        ])
        .map_err(|e| format!("Failed to write fusion score: {e}"))?;
    }

    Ok(())
}

/// Prune telemetry rows older than retention_days.
pub fn prune_old_runs(conn: &Connection, retention_days: u32) -> Result<u64, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let cutoff_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(retention_days as u64 * 86400);

    // Delete candidate_scores first (FK) — numeric comparison on epoch seconds
    let candidate_deleted: u64 =
        conn.execute(
            "DELETE FROM candidate_scores WHERE run_id IN (
                SELECT run_id FROM retrieval_runs WHERE CAST(timestamp AS INTEGER) < ?1
            )",
            params![cutoff_secs],
        )
        .map_err(|e| format!("Failed to prune candidate_scores: {e}"))? as u64;

    // Delete fusion_scores
    let fusion_deleted: u64 =
        conn.execute(
            "DELETE FROM fusion_scores WHERE run_id IN (
                SELECT run_id FROM retrieval_runs WHERE CAST(timestamp AS INTEGER) < ?1
            )",
            params![cutoff_secs],
        )
        .map_err(|e| format!("Failed to prune fusion_scores: {e}"))? as u64;

    // Delete retrieval_runs
    let runs_deleted: u64 =
        conn.execute(
            "DELETE FROM retrieval_runs WHERE CAST(timestamp AS INTEGER) < ?1",
            params![cutoff_secs],
        )
        .map_err(|e| format!("Failed to prune retrieval_runs: {e}"))? as u64;

    Ok(candidate_deleted + fusion_deleted + runs_deleted)
}

/// A candidate score row for telemetry.
pub struct CandidateScoreRow {
    pub chunk_id: Option<String>,
    pub source_lane: String,
    pub raw_rank: u32,
    pub raw_score: f32,
    pub normalized_score: f32,
    pub is_exact_hit: bool,
    pub exact_hit_floor_applied: bool,
}

/// A fusion score row for telemetry.
pub struct FusionScoreRow {
    pub chunk_id: Option<String>,
    pub rrf_score: f32,
    pub exact_hit_floor_applied: bool,
    pub final_score: f32,
    pub provenance_json: Option<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory_conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn schema_creates_all_tables() {
        let conn = in_memory_conn();
        init_telemetry_schema(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"retrieval_runs".to_string()));
        assert!(tables.contains(&"candidate_scores".to_string()));
        assert!(tables.contains(&"fusion_scores".to_string()));
        assert!(tables.contains(&"telemetry_meta".to_string()));
    }

    #[test]
    fn hash_query_is_deterministic() {
        let salt = "test-salt";
        let h1 = hash_query("hello world", salt);
        let h2 = hash_query("hello world", salt);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_query_differs_with_different_salt() {
        let h1 = hash_query("hello", "salt1");
        let h2 = hash_query("hello", "salt2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn write_retrieval_run_populates_table() {
        let conn = in_memory_conn();
        init_telemetry_schema(&conn).unwrap();

        let config = TelemetryConfig::default();
        let run_id = write_retrieval_run(
            &conn,
            &config,
            "test query",
            "natural_language",
            42.0,
            "agent_fast",
            "{}",
            false,
            None,
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM retrieval_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (hash, raw): (String, Option<String>) = conn
            .query_row(
                "SELECT query_hash, query_raw FROM retrieval_runs WHERE run_id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(!hash.is_empty());
        assert!(raw.is_none()); // AC-4: query_raw IS NULL by default
    }

    #[test]
    fn raw_mode_populates_query_raw() {
        let conn = in_memory_conn();
        init_telemetry_schema(&conn).unwrap();

        let config = TelemetryConfig {
            telemetry_store_query: "raw".to_string(),
            ..Default::default()
        };

        let run_id = write_retrieval_run(
            &conn,
            &config,
            "secret query",
            "natural_language",
            42.0,
            "agent_fast",
            "{}",
            false,
            None,
        )
        .unwrap();

        let raw: Option<String> = conn
            .query_row(
                "SELECT query_raw FROM retrieval_runs WHERE run_id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw.as_deref(), Some("secret query"));
    }

    #[test]
    fn prune_removes_old_rows() {
        let conn = in_memory_conn();
        init_telemetry_schema(&conn).unwrap();

        // Insert with a very old timestamp (year 2000)
        conn.execute(
            "INSERT INTO retrieval_runs (
                run_id, query_hash, query_raw, query_kind, timestamp,
                latency_ms, profile, backend_config, context_exhausted, reranker_skipped_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                "old-run",
                "hash",
                None::<String>,
                "natural_language",
                "946684800", // 2000-01-01 in epoch seconds
                42.0,
                "agent_fast",
                "{}",
                0,
                None::<String>,
            ],
        )
        .unwrap();

        let deleted = prune_old_runs(&conn, 1).unwrap();
        assert!(deleted > 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM retrieval_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = in_memory_conn();
        init_telemetry_schema(&conn).unwrap();
        init_telemetry_schema(&conn).unwrap();
    }
}
