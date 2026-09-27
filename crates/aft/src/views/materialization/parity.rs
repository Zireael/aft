//! Logical equality of two derived databases, used to prove that an
//! incremental materialization equals a cold build of the same manifest.
//!
//! Two databases are logically equal when they have the same schema (every
//! table, index, view and trigger with identical SQL, excluding SQLite's own
//! `sqlite_%` objects) and, for every table in either database, the same
//! multiset of rows. Values are compared with their storage class, so
//! `INTEGER 1`, `REAL 1.0` and `TEXT '1'` all differ. Implicit rowids are
//! excluded because insertion order legitimately differs; page layout,
//! freelists and WAL contents are outside the comparison.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::Connection;

/// Snapshot key holding the schema rows. SQLite reserves the `sqlite_`
/// prefix, so no user table can have this name.
pub(crate) const SCHEMA_KEY: &str = "sqlite_schema";

/// Sorted rows per table, plus the schema under [`SCHEMA_KEY`].
pub(crate) type LogicalSnapshot = BTreeMap<String, Vec<String>>;

fn typed(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(value) => format!("INTEGER {value}"),
        // Bit pattern, so -0.0 and 0.0 or two NaN payloads are not conflated.
        ValueRef::Real(value) => format!("REAL {:016x}", value.to_bits()),
        ValueRef::Text(bytes) => format!("TEXT {}", String::from_utf8_lossy(bytes)),
        ValueRef::Blob(bytes) => {
            let hex = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            format!("BLOB {hex}")
        }
    }
}

pub(crate) fn logical_snapshot(path: &Path) -> LogicalSnapshot {
    let connection = Connection::open(path).unwrap();
    let mut snapshot = LogicalSnapshot::new();
    let mut schema = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'",
        )
        .unwrap()
        .query_map([], |row| {
            Ok(format!(
                "{:?}",
                (0..4)
                    .map(|index| row.get_ref(index).map(typed))
                    .collect::<rusqlite::Result<Vec<_>>>()?
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    schema.sort();
    snapshot.insert(SCHEMA_KEY.to_string(), schema);
    let tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for table in tables {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                Ok(format!(
                    "{:?}",
                    (0..columns)
                        .map(|index| row.get_ref(index).map(typed))
                        .collect::<rusqlite::Result<Vec<_>>>()?
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows.sort();
        snapshot.insert(table, rows);
    }
    snapshot
}

/// Every difference between two snapshots, over the union of their keys.
/// Rows compare as multisets: both sides are sorted, and duplicates count.
pub(crate) fn snapshot_differences(
    expected: &LogicalSnapshot,
    actual: &LogicalSnapshot,
) -> Vec<String> {
    let mut differences = Vec::new();
    let keys = expected
        .keys()
        .chain(actual.keys())
        .collect::<BTreeSet<_>>();
    for key in keys {
        let label = if key == SCHEMA_KEY {
            "schema".to_string()
        } else {
            format!("table {key}")
        };
        let (Some(expected_rows), Some(actual_rows)) = (expected.get(key), actual.get(key)) else {
            differences.push(format!(
                "{label}: only in the {} database",
                if expected.contains_key(key) {
                    "expected"
                } else {
                    "actual"
                }
            ));
            continue;
        };
        if expected_rows == actual_rows {
            continue;
        }
        let (missing, extra) = multiset_difference(expected_rows, actual_rows);
        let clip =
            |row: Option<&&String>| row.map(|row| row.chars().take(1000).collect::<String>());
        differences.push(format!(
            "{label}: missing={} extra={}; first missing={:?}; first extra={:?}",
            missing.len(),
            extra.len(),
            clip(missing.first()),
            clip(extra.first()),
        ));
    }
    differences
}

/// Rows of `expected` not matched in `actual`, and the reverse, both sorted.
fn multiset_difference<'a>(
    expected: &'a [String],
    actual: &'a [String],
) -> (Vec<&'a String>, Vec<&'a String>) {
    let (mut missing, mut extra) = (Vec::new(), Vec::new());
    let (mut left, mut right) = (expected.iter().peekable(), actual.iter().peekable());
    loop {
        match (left.peek(), right.peek()) {
            (Some(a), Some(b)) if a == b => {
                left.next();
                right.next();
            }
            (Some(a), Some(b)) if a < b => missing.extend(left.next()),
            (Some(_), Some(_)) => extra.extend(right.next()),
            (Some(_), None) => missing.extend(left.next()),
            (None, Some(_)) => extra.extend(right.next()),
            (None, None) => return (missing, extra),
        }
    }
}

pub(crate) fn assert_snapshots_equal(expected: &LogicalSnapshot, actual: &LogicalSnapshot) {
    let differences = snapshot_differences(expected, actual);
    assert!(
        differences.is_empty(),
        "derived databases differ:\n{}",
        differences.join("\n")
    );
}

pub(crate) fn assert_logical_parity(actual: &Path, expected: &Path) {
    assert_snapshots_equal(&logical_snapshot(expected), &logical_snapshot(actual));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(directory: &Path, name: &str) -> std::path::PathBuf {
        let path = directory.join(name);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE refs (ref_id TEXT PRIMARY KEY, caller_file TEXT, line INTEGER);
                 CREATE INDEX idx_refs_caller_file ON refs(caller_file);
                 INSERT INTO refs VALUES ('r1', 'a.ts', 1), ('r2', 'b.ts', 2);",
            )
            .unwrap();
        path
    }

    /// The comparison `materialization::tests` used before: sorted typed rows
    /// of every table named in the expected database only.
    fn expected_tables_only_equal(expected: &LogicalSnapshot, actual: &LogicalSnapshot) -> bool {
        expected
            .iter()
            .filter(|(key, _)| key.as_str() != SCHEMA_KEY)
            .all(|(table, rows)| actual.get(table) == Some(rows))
    }

    #[test]
    fn identical_databases_have_no_differences() {
        let directory = tempfile::tempdir().unwrap();
        let expected = database(directory.path(), "expected.sqlite");
        let actual = database(directory.path(), "actual.sqlite");
        assert_eq!(
            snapshot_differences(&logical_snapshot(&expected), &logical_snapshot(&actual)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn extra_table_in_the_actual_database_is_a_difference() {
        let directory = tempfile::tempdir().unwrap();
        let expected = database(directory.path(), "expected.sqlite");
        let actual = database(directory.path(), "actual.sqlite");
        Connection::open(&actual)
            .unwrap()
            .execute_batch("CREATE TABLE planted (value TEXT);")
            .unwrap();
        let (expected, actual) = (logical_snapshot(&expected), logical_snapshot(&actual));

        assert!(
            expected_tables_only_equal(&expected, &actual),
            "the old comparison missed the planted table"
        );
        let differences = snapshot_differences(&expected, &actual);
        assert!(
            differences
                .iter()
                .any(|line| line == "table planted: only in the actual database"),
            "{differences:?}"
        );
        assert!(differences.iter().any(|line| line.starts_with("schema:")));
    }

    #[test]
    fn missing_index_is_a_difference() {
        let directory = tempfile::tempdir().unwrap();
        let expected = database(directory.path(), "expected.sqlite");
        let actual = database(directory.path(), "actual.sqlite");
        Connection::open(&actual)
            .unwrap()
            .execute_batch("DROP INDEX idx_refs_caller_file;")
            .unwrap();
        let (expected, actual) = (logical_snapshot(&expected), logical_snapshot(&actual));

        assert!(
            expected_tables_only_equal(&expected, &actual),
            "the old comparison missed the dropped index"
        );
        let differences = snapshot_differences(&expected, &actual);
        assert_eq!(differences.len(), 1, "{differences:?}");
        assert!(
            differences[0].starts_with("schema: missing=1 extra=0")
                && differences[0].contains("idx_refs_caller_file"),
            "{differences:?}"
        );
    }

    #[test]
    fn storage_class_and_duplicate_rows_are_differences() {
        let directory = tempfile::tempdir().unwrap();
        let schema = "CREATE TABLE meta (k TEXT, v);";
        let expected = directory.path().join("expected.sqlite");
        let actual = directory.path().join("actual.sqlite");
        Connection::open(&expected)
            .unwrap()
            .execute_batch(&format!("{schema} INSERT INTO meta VALUES ('n', 1);"))
            .unwrap();
        Connection::open(&actual)
            .unwrap()
            .execute_batch(&format!("{schema} INSERT INTO meta VALUES ('n', '1');"))
            .unwrap();
        assert_eq!(
            snapshot_differences(&logical_snapshot(&expected), &logical_snapshot(&actual)).len(),
            1
        );
        Connection::open(&actual)
            .unwrap()
            .execute_batch("DELETE FROM meta; INSERT INTO meta VALUES ('n', 1), ('n', 1);")
            .unwrap();
        let differences =
            snapshot_differences(&logical_snapshot(&expected), &logical_snapshot(&actual));
        assert!(
            differences.len() == 1 && differences[0].starts_with("table meta: missing=0 extra=1"),
            "{differences:?}"
        );
    }
}
