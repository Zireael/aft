use serde_json::json;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

pub fn run(args: Vec<OsString>) -> Result<(), TelemetryError> {
    let args = TelemetryArgs::parse(args)?;
    if args.help {
        print_usage();
        return Ok(());
    }

    match args.command.as_deref() {
        Some("prune") => prune(args),
        Some(command) => Err(TelemetryError::usage(format!(
            "unknown telemetry command: {command}"
        ))),
        None => Err(TelemetryError::usage("missing telemetry command: prune")),
    }
}

fn prune(args: TelemetryArgs) -> Result<(), TelemetryError> {
    let storage_dir = args
        .storage_dir
        .unwrap_or_else(|| aft::bash_background::storage_dir(None));
    let db_path = storage_dir.join("aft.db");
    let conn = aft::db::open(&db_path).map_err(|error| {
        TelemetryError::runtime(format!(
            "failed to open telemetry database at {}: {error}",
            db_path.display()
        ))
    })?;
    aft::telemetry::init_telemetry_schema(&conn).map_err(TelemetryError::runtime)?;
    let retention_days = args.retention_days.unwrap_or(30);
    let deleted =
        aft::telemetry::prune_old_runs(&conn, retention_days).map_err(TelemetryError::runtime)?;

    println!(
        "{}",
        json!({
            "success": true,
            "command": "telemetry prune",
            "database": db_path.display().to_string(),
            "retention_days": retention_days,
            "deleted_rows": deleted,
        })
    );
    Ok(())
}

#[derive(Debug)]
pub struct TelemetryError {
    message: String,
    exit_code: i32,
}

impl TelemetryError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: 2,
        }
    }

    fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: 1,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TelemetryError {}

#[derive(Debug)]
struct TelemetryArgs {
    command: Option<String>,
    storage_dir: Option<PathBuf>,
    retention_days: Option<u32>,
    help: bool,
}

impl TelemetryArgs {
    fn parse(args: Vec<OsString>) -> Result<Self, TelemetryError> {
        let mut parsed = Self {
            command: None,
            storage_dir: None,
            retention_days: None,
            help: false,
        };

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            let Some(arg) = arg.to_str() else {
                return Err(TelemetryError::usage("arguments must be valid UTF-8"));
            };
            match arg {
                "--help" | "-h" => parsed.help = true,
                "prune" if parsed.command.is_none() => {
                    parsed.command = Some("prune".to_string());
                }
                "--storage-dir" => {
                    let value = next_value(&mut iter, "--storage-dir")?;
                    parsed.storage_dir = Some(PathBuf::from(value));
                }
                "--retention-days" | "--days" => {
                    let value = next_value(&mut iter, arg)?;
                    let days = value.parse::<u32>().map_err(|_| {
                        TelemetryError::usage(format!("{arg} must be an unsigned integer"))
                    })?;
                    parsed.retention_days = Some(days);
                }
                other if parsed.command.is_none() => {
                    return Err(TelemetryError::usage(format!(
                        "unknown telemetry command: {other}"
                    )));
                }
                other => {
                    return Err(TelemetryError::usage(format!(
                        "unknown telemetry argument: {other}"
                    )));
                }
            }
        }

        Ok(parsed)
    }
}

fn next_value(
    iter: &mut impl Iterator<Item = OsString>,
    flag: &str,
) -> Result<String, TelemetryError> {
    let value = iter
        .next()
        .ok_or_else(|| TelemetryError::usage(format!("{flag} requires a value")))?;
    value
        .into_string()
        .map_err(|_| TelemetryError::usage(format!("{flag} value must be valid UTF-8")))
}

fn print_usage() {
    println!(
        "Usage: aft telemetry prune [--storage-dir <path>] [--retention-days <days>]\n\
         Prunes retrieval telemetry rows older than the retention window."
    );
}
