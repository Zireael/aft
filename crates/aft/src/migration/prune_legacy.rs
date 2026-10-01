//! `aft cache prune-legacy`: the only path that deletes a legacy (pre-view)
//! artifact set.
//!
//! Nothing deletes legacy sets automatically: not startup, not a sweep, not
//! maintenance. The operator runs this command, which is a dry run unless
//! `--yes` is given. It lists each legacy set as a whole with its bytes and
//! prunes a set only when the set's import into per-checkout views completed
//! and the retention window (14 days) has passed since, so an offline
//! rollback to the previous binary keeps its caches in between.
//!
//! The process census is a guard against a mistake, not process exclusion:
//!
//! 1. Census. Any other `aft`/`ck-aft` process, an AFT-like process it cannot
//!    classify, or a census that fails refuses the whole run.
//! 2. Rename aside. Each eligible set is moved, one atomic `rename(2)` per
//!    directory, into `<storage>/.prune-legacy-<timestamp>/` on the same
//!    filesystem. An opener after the rename finds no set and treats it as
//!    absent; one that held files open keeps its descriptors on the moved
//!    inodes. A rename that is refused or cannot be atomic (another
//!    filesystem, or Windows) skips the set; nothing is ever deleted in place.
//! 3. Census again. If a process appeared, the moved directory is kept and
//!    reported and the command exits non-zero. Otherwise it is deleted,
//!    together with any `.prune-legacy-*` leftovers of earlier runs.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::per_checkout::{validate_legacy_key, ImportLedger, LEGACY_RETENTION};

/// The operator contract, printed by the command's help and by every run
/// before its census.
pub const OPERATOR_CONTRACT: &str = "Stop every AFT process (the daemon, OpenCode and Pi hosts) before running this. The check below refuses when it sees one, but it can't stop a process that starts after the check.";

/// Directory name prefix of a run's rename-aside area under the storage root.
pub const PRUNE_DIR_PREFIX: &str = ".prune-legacy-";

/// Upper bound on legacy keys read from one legacy root.
const MAX_KEYS_PER_ROOT: usize = 10_000;
/// Upper bound on filesystem entries counted inside one directory tree.
const MAX_ENTRIES_PER_TREE: usize = 500_000;

/// One directory family of a legacy set.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LegacyKind {
    Index,
    Semantic,
    Callgraph,
    ArtifactOwners,
}

impl LegacyKind {
    pub const ALL: [Self; 4] = [
        Self::Index,
        Self::Semantic,
        Self::Callgraph,
        Self::ArtifactOwners,
    ];

    /// The legacy root under the storage directory.
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Semantic => "semantic",
            Self::Callgraph => "callgraph",
            Self::ArtifactOwners => "artifact-owners",
        }
    }

    /// Entries of a legacy root that are not legacy sets: the embedding
    /// model cache lives in `semantic/models`.
    fn reserved(self, name: &str) -> bool {
        matches!((self, name), (Self::Semantic, "models"))
    }
}

/// One directory of a legacy set with its inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyDir {
    pub kind: LegacyKind,
    pub path: PathBuf,
    pub bytes: u64,
    pub entries: u64,
}

/// Why a legacy set is or is not pruned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Eligibility {
    /// Import completed at `completed_ms` and the retention window passed.
    Eligible { completed_ms: u64 },
    /// No import was ever recorded for this key.
    NotImported,
    /// The import started but some artifact is not done or rejected yet.
    ImportIncomplete,
    /// The import completed; the set is kept until `eligible_at_ms`.
    Retained { eligible_at_ms: u64 },
    /// The import ledger could not be read; uncertainty keeps the set.
    LedgerUnreadable(String),
    /// The set could not be inventoried completely; it is never moved.
    InventoryIncomplete(String),
}

impl Eligibility {
    pub fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible { .. })
    }

    fn describe(&self) -> String {
        match self {
            Self::Eligible { completed_ms } => {
                format!("eligible: import completed {}", format_ms(*completed_ms))
            }
            Self::NotImported => "kept: never imported into per-checkout views".to_owned(),
            Self::ImportIncomplete => "kept: its import has not completed".to_owned(),
            Self::Retained { eligible_at_ms } => format!(
                "kept: retained for rollback until {}",
                format_ms(*eligible_at_ms)
            ),
            Self::LedgerUnreadable(reason) => format!("kept: import ledger unreadable: {reason}"),
            Self::InventoryIncomplete(reason) => format!("kept: inventory incomplete: {reason}"),
        }
    }
}

/// Every directory a legacy key owns, with its eligibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacySet {
    pub key: String,
    pub dirs: Vec<LegacyDir>,
    pub eligibility: Eligibility,
}

impl LegacySet {
    pub fn bytes(&self) -> u64 {
        self.dirs.iter().map(|dir| dir.bytes).sum()
    }
}

/// A rename-aside directory left by an earlier run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Leftover {
    pub path: PathBuf,
    pub bytes: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    pub sets: Vec<LegacySet>,
    pub leftovers: Vec<Leftover>,
    /// A legacy root held more keys than one run reads.
    pub truncated: Vec<LegacyKind>,
}

/// Size of a directory tree without following symlinks. The walk stops at
/// `limit` entries; `complete` is false when it did.
fn tree_size(path: &Path, limit: usize) -> io::Result<(u64, u64, bool)> {
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    let mut stack = vec![path.to_path_buf()];
    let mut budget = limit;
    while let Some(dir) = stack.pop() {
        let metadata = fs::symlink_metadata(&dir)?;
        bytes += metadata.len();
        if !metadata.is_dir() {
            entries += 1;
            continue;
        }
        let mut children = fs::read_dir(&dir)?.take(budget.saturating_add(1));
        for child in children.by_ref() {
            if budget == 0 {
                return Ok((bytes, entries, false));
            }
            budget -= 1;
            let child = child?;
            let file_type = child.file_type()?;
            if file_type.is_dir() {
                stack.push(child.path());
            } else {
                bytes += child.metadata()?.len();
                entries += 1;
            }
        }
    }
    Ok((bytes, entries, true))
}

fn now_ms(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn format_ms(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Decides eligibility from the key's import ledger, read without writing.
fn eligibility(storage: &Path, key: &str, now: SystemTime) -> Eligibility {
    let status = match ImportLedger::read_status(storage, key) {
        Ok(Some(status)) => status,
        Ok(None) => return Eligibility::NotImported,
        Err(error) => return Eligibility::LedgerUnreadable(error.to_string()),
    };
    let (true, Some(completed_ms)) = (status.complete, status.completed_ms) else {
        return Eligibility::ImportIncomplete;
    };
    let retention_ms = LEGACY_RETENTION.as_millis() as u64;
    let eligible_at_ms = completed_ms.saturating_add(retention_ms);
    if now_ms(now) >= eligible_at_ms {
        Eligibility::Eligible { completed_ms }
    } else {
        Eligibility::Retained { eligible_at_ms }
    }
}

/// Lists every legacy set and rename-aside leftover under `storage`.
pub fn inventory(storage: &Path, now: SystemTime) -> io::Result<Inventory> {
    let mut inventory = Inventory::default();
    let mut by_key: std::collections::BTreeMap<String, Vec<LegacyDir>> = Default::default();
    let mut incomplete: std::collections::BTreeMap<String, String> = Default::default();
    for kind in LegacyKind::ALL {
        let root = storage.join(kind.dir_name());
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut seen = 0;
        for entry in entries.take(MAX_KEYS_PER_ROOT + 1) {
            seen += 1;
            if seen > MAX_KEYS_PER_ROOT {
                inventory.truncated.push(kind);
                break;
            }
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if kind.reserved(&name)
                || validate_legacy_key(&name).is_err()
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let path = entry.path();
            let (bytes, count) = match tree_size(&path, MAX_ENTRIES_PER_TREE) {
                Ok((bytes, count, true)) => (bytes, count),
                Ok((bytes, count, false)) => {
                    incomplete.insert(
                        name.clone(),
                        format!(
                            "{} holds more than {MAX_ENTRIES_PER_TREE} entries",
                            path.display()
                        ),
                    );
                    (bytes, count)
                }
                Err(error) => {
                    incomplete.insert(
                        name.clone(),
                        format!("{} could not be read: {error}", path.display()),
                    );
                    (0, 0)
                }
            };
            by_key.entry(name).or_default().push(LegacyDir {
                kind,
                path,
                bytes,
                entries: count,
            });
        }
    }
    for (key, dirs) in by_key {
        let eligibility = match incomplete.remove(&key) {
            Some(reason) => Eligibility::InventoryIncomplete(reason),
            None => eligibility(storage, &key, now),
        };
        inventory.sets.push(LegacySet {
            key,
            dirs,
            eligibility,
        });
    }
    if let Ok(entries) = fs::read_dir(storage) {
        for entry in entries.take(MAX_KEYS_PER_ROOT) {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(PRUNE_DIR_PREFIX) {
                continue;
            }
            let path = entry.path();
            let (bytes, _, complete) =
                tree_size(&path, MAX_ENTRIES_PER_TREE).unwrap_or((0, 0, false));
            inventory.leftovers.push(Leftover {
                path,
                bytes,
                complete,
            });
        }
        inventory
            .leftovers
            .sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok(inventory)
}

// ---------------------------------------------------------------------------
// Process census

/// One process whose presence stops the prune.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CensusFinding {
    /// An `aft` or `ck-aft` process; `daemon` is true when it was started
    /// with `--subc`.
    Aft {
        pid: u32,
        daemon: bool,
        command: String,
    },
    /// A process whose name mentions AFT but is not an `aft` or `ck-aft`
    /// executable.
    Unclassifiable {
        pid: u32,
        command: String,
        reason: String,
    },
}

impl CensusFinding {
    fn describe(&self) -> String {
        match self {
            Self::Aft {
                pid,
                daemon: true,
                command,
            } => format!("live AFT daemon pid {pid}: {command}"),
            Self::Aft { pid, command, .. } => format!("live AFT process pid {pid}: {command}"),
            Self::Unclassifiable {
                pid,
                command,
                reason,
            } => format!("unclassifiable process pid {pid} ({reason}): {command}"),
        }
    }
}

/// Lists the processes that block a prune, excluding the process running it.
pub trait ProcessCensus {
    fn take(&self) -> Result<Vec<CensusFinding>, String>;
}

/// The census of this machine, from `ps`.
pub struct SystemCensus;

impl ProcessCensus for SystemCensus {
    #[cfg(unix)]
    fn take(&self) -> Result<Vec<CensusFinding>, String> {
        let output = std::process::Command::new("ps")
            .args(["-axww", "-o", "pid=,comm=,args="])
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| format!("could not run ps: {error}"))?;
        if !output.status.success() {
            return Err(format!("ps exited with {}", output.status));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| "ps printed bytes that are not UTF-8".to_owned())?;
        parse_ps_output(&text, std::process::id())
    }

    #[cfg(not(unix))]
    fn take(&self) -> Result<Vec<CensusFinding>, String> {
        Err("the process census is not available on this platform".to_owned())
    }
}

const AFT_NAMES: [&str; 4] = ["aft", "ck-aft", "aft.exe", "ck-aft.exe"];

fn basename(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

/// True when a name has `aft` as one of its words (`aft-bridge`, `my_aft`),
/// which `craft` or `draft` do not.
fn names_aft(value: &str) -> bool {
    basename(value)
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == "aft")
}

/// Classifies `ps -o pid=,comm=,args=` output. An `aft`/`ck-aft` executable
/// is a holder; a process whose executable or first argument names AFT any
/// other way cannot be classified; a line that does not parse is a census
/// error.
pub fn parse_ps_output(output: &str, self_pid: u32) -> Result<Vec<CensusFinding>, String> {
    let mut findings = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (pid, rest) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("unparsable ps line: {line}"))?;
        let pid: u32 = pid
            .parse()
            .map_err(|_| format!("unparsable ps pid in: {line}"))?;
        if pid == self_pid {
            continue;
        }
        let rest = rest.trim_start();
        let (comm, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let args = args.trim();
        let argv0 = args.split_whitespace().next().unwrap_or("");
        let exact = |value: &str| AFT_NAMES.contains(&basename(value));
        let command = if args.is_empty() { comm } else { args }.to_owned();
        if exact(comm) || exact(argv0) {
            let daemon = args
                .split_whitespace()
                .any(|arg| arg == "--subc" || arg.starts_with("--subc="));
            findings.push(CensusFinding::Aft {
                pid,
                daemon,
                command,
            });
        } else if names_aft(comm) || names_aft(argv0) {
            findings.push(CensusFinding::Unclassifiable {
                pid,
                command,
                reason: "its name mentions AFT but is not an aft or ck-aft executable".to_owned(),
            });
        }
    }
    Ok(findings)
}

// ---------------------------------------------------------------------------
// The prune

/// Points a test can hold the run at to simulate races.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PruneStep<'a> {
    /// The first census came back clean; no set has been moved.
    CensusClean,
    /// A set's directories were renamed aside.
    SetMoved(&'a str),
    /// Every set was handled; the second census comes next.
    BeforeRecensus,
}

pub trait PruneObserver {
    fn reached(&self, step: PruneStep<'_>);
}

/// How a run ended. The numeric value is the process exit code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PruneExit {
    /// Dry run, or every eligible set and leftover was deleted.
    Clean = 0,
    /// The census saw a live holder, could not classify a process, or failed.
    Refused = 3,
    /// A process appeared after sets were renamed aside; they are kept.
    ProcessAppeared = 4,
    /// A set could not be renamed atomically and was skipped.
    Skipped = 5,
}

impl PruneExit {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Debug)]
pub struct PruneOptions {
    /// Delete eligible sets instead of listing them.
    pub yes: bool,
    pub now: SystemTime,
}

fn census_refusal(census: &dyn ProcessCensus) -> Option<Vec<String>> {
    match census.take() {
        Err(error) => Some(vec![format!("the process census failed: {error}")]),
        Ok(findings) if findings.is_empty() => None,
        Ok(findings) => Some(findings.iter().map(CensusFinding::describe).collect()),
    }
}

/// Whether a rename from `source` into `target_dir` stays on one filesystem,
/// which is what makes it a single atomic `rename(2)`.
#[cfg(unix)]
fn same_filesystem(source: &Path, target_dir: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(fs::symlink_metadata(source)?.dev() == fs::metadata(target_dir)?.dev())
}

// Only Unix renames directories here, so elsewhere every set is skipped.
#[cfg_attr(not(unix), allow(dead_code))]
enum Moved {
    All,
    Skipped(String),
    /// Some directories moved, the rest refused, and moving them back failed:
    /// the rename-aside directory must not be deleted, or the set would be
    /// removed in part.
    Partial(String),
}

/// Renames every directory of `set` into `area/<key>/<kind>`, or none of them.
fn move_set(set: &LegacySet, area: &Path) -> io::Result<Moved> {
    #[cfg(not(unix))]
    {
        let _ = (set, area);
        return Ok(Moved::Skipped(
            "directories cannot be renamed atomically on this platform".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        for dir in &set.dirs {
            if !same_filesystem(&dir.path, area)? {
                return Ok(Moved::Skipped(format!(
                    "{} is on another filesystem than the storage root",
                    dir.path.display()
                )));
            }
        }
        let target = area.join(&set.key);
        fs::create_dir(&target)?;
        let mut moved: Vec<(&Path, PathBuf)> = Vec::new();
        for dir in &set.dirs {
            let destination = target.join(dir.kind.dir_name());
            if let Err(error) = fs::rename(&dir.path, &destination) {
                let reason = format!("rename of {} refused: {error}", dir.path.display());
                for (source, destination) in moved.iter().rev() {
                    if let Err(back) = fs::rename(destination, source) {
                        return Ok(Moved::Partial(format!(
                            "{reason}; moving {} back failed: {back}",
                            destination.display()
                        )));
                    }
                }
                let _ = fs::remove_dir(&target);
                return Ok(Moved::Skipped(reason));
            }
            moved.push((&dir.path, destination));
        }
        Ok(Moved::All)
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {} ({bytes} bytes)", UNITS[unit])
    }
}

/// Runs the command against `storage`. Output is written to `out`.
pub fn prune_legacy(
    storage: &Path,
    options: &PruneOptions,
    census: &dyn ProcessCensus,
    observer: Option<&dyn PruneObserver>,
    out: &mut dyn Write,
) -> io::Result<PruneExit> {
    writeln!(out, "{OPERATOR_CONTRACT}")?;
    writeln!(out)?;
    let inventory = inventory(storage, options.now)?;
    write_inventory(out, storage, &inventory)?;

    if let Some(reasons) = census_refusal(census) {
        writeln!(out, "Refusing: the storage root may be in use.")?;
        for reason in reasons {
            writeln!(out, "  {reason}")?;
        }
        return Ok(PruneExit::Refused);
    }
    writeln!(out, "Process census: no other AFT process found.")?;
    if let Some(observer) = observer {
        observer.reached(PruneStep::CensusClean);
    }

    let eligible = inventory
        .sets
        .iter()
        .filter(|set| set.eligibility.is_eligible())
        .collect::<Vec<_>>();
    if !options.yes {
        let bytes: u64 = eligible.iter().map(|set| set.bytes()).sum();
        writeln!(
            out,
            "Dry run: {} legacy set(s), {}, would be pruned; {} leftover director{} would be removed. Pass --yes to prune.",
            eligible.len(),
            human_bytes(bytes),
            inventory.leftovers.len(),
            if inventory.leftovers.len() == 1 { "y" } else { "ies" }
        )?;
        return Ok(PruneExit::Clean);
    }
    if eligible.is_empty() && inventory.leftovers.is_empty() {
        writeln!(out, "Nothing to prune.")?;
        return Ok(PruneExit::Clean);
    }

    let mut area = None;
    let mut skipped = false;
    let mut keep_area = false;
    let mut moved_bytes = 0_u64;
    if !eligible.is_empty() {
        let millis = now_ms(SystemTime::now());
        let path = storage.join(format!("{PRUNE_DIR_PREFIX}{millis}-{}", std::process::id()));
        fs::create_dir(&path)?;
        for set in &eligible {
            match move_set(set, &path)? {
                Moved::All => {
                    moved_bytes += set.bytes();
                    writeln!(
                        out,
                        "Moved {} aside ({}).",
                        set.key,
                        human_bytes(set.bytes())
                    )?;
                    if let Some(observer) = observer {
                        observer.reached(PruneStep::SetMoved(&set.key));
                    }
                }
                Moved::Skipped(reason) => {
                    skipped = true;
                    writeln!(out, "Skipped {}: {reason}. Nothing was deleted.", set.key)?;
                }
                Moved::Partial(reason) => {
                    skipped = true;
                    keep_area = true;
                    writeln!(
                        out,
                        "Skipped {}: {reason}. Its moved part stays in {}.",
                        set.key,
                        path.display()
                    )?;
                }
            }
        }
        area = Some(path);
    }
    if let Some(observer) = observer {
        observer.reached(PruneStep::BeforeRecensus);
    }

    if let Some(reasons) = census_refusal(census) {
        writeln!(
            out,
            "A process appeared after the first check; nothing was deleted."
        )?;
        for reason in reasons {
            writeln!(out, "  {reason}")?;
        }
        if let Some(area) = &area {
            writeln!(
                out,
                "Kept the moved sets in {} ({}). A later clean run removes them.",
                area.display(),
                human_bytes(moved_bytes)
            )?;
        }
        return Ok(PruneExit::ProcessAppeared);
    }

    let mut freed = 0_u64;
    if let Some(area) = &area {
        if keep_area {
            writeln!(
                out,
                "Kept {} because a set moved only in part.",
                area.display()
            )?;
        } else {
            fs::remove_dir_all(area)?;
            freed += moved_bytes;
        }
    }
    for leftover in &inventory.leftovers {
        fs::remove_dir_all(&leftover.path)?;
        freed += leftover.bytes;
        writeln!(out, "Removed leftover {}.", leftover.path.display())?;
    }
    writeln!(out, "Pruned {}.", human_bytes(freed))?;
    Ok(if skipped {
        PruneExit::Skipped
    } else {
        PruneExit::Clean
    })
}

fn write_inventory(out: &mut dyn Write, storage: &Path, inventory: &Inventory) -> io::Result<()> {
    writeln!(out, "Legacy sets under {}:", storage.display())?;
    if inventory.sets.is_empty() {
        writeln!(out, "  (none)")?;
    }
    for set in &inventory.sets {
        writeln!(
            out,
            "  {}  {}  {}",
            set.key,
            human_bytes(set.bytes()),
            set.eligibility.describe()
        )?;
        for dir in &set.dirs {
            writeln!(
                out,
                "    {}/  {} in {} entries",
                dir.kind.dir_name(),
                human_bytes(dir.bytes),
                dir.entries
            )?;
        }
    }
    for kind in &inventory.truncated {
        writeln!(
            out,
            "  {}/ holds more than {MAX_KEYS_PER_ROOT} entries; only the first were read.",
            kind.dir_name()
        )?;
    }
    for leftover in &inventory.leftovers {
        writeln!(
            out,
            "  leftover {}  {}{}",
            leftover
                .path
                .file_name()
                .unwrap_or_else(|| OsStr::new("?"))
                .to_string_lossy(),
            human_bytes(leftover.bytes),
            if leftover.complete {
                ""
            } else {
                " (partial count)"
            }
        )?;
    }
    writeln!(out)?;
    Ok(())
}

/// The retention window as a whole number of days, for help text.
pub fn retention_days() -> u64 {
    LEGACY_RETENTION.as_secs() / Duration::from_secs(86_400).as_secs()
}

#[cfg(test)]
#[path = "prune_legacy_tests.rs"]
mod tests;
