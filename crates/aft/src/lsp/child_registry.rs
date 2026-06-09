//! Process-wide registry of LSP child PIDs spawned by `LspClient::spawn`.
//!
//! Mirrors the `BgTaskRegistry` pattern: `Arc`-cloneable handle that the
//! signal handler thread can use to SIGKILL all child language servers
//! before the aft process exits. Without this registry, LSP children get
//! orphaned to PID 1 when aft is SIGTERM'd by its parent (e.g., during
//! plugin bridge.shutdown() or e2e test cleanup), accumulating across runs.
//!
//! The registry intentionally does NOT do graceful shutdown — that takes
//! up to 5 seconds per server (shutdown request + exit notification +
//! poll). Signal handlers must finish quickly. Graceful shutdown still
//! happens on the natural stdin-closed exit path via `LspManager::shutdown_all`.

use std::collections::HashSet;
use std::io;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct LspChildRegistry {
    inner: Arc<Mutex<HashSet<u32>>>,
}

impl LspChildRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track a newly-spawned LSP child PID.
    pub fn track(&self, pid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.insert(pid);
        }
    }

    /// Spawn a child while holding the same mutex used by signal cleanup, then
    /// insert its PID before releasing that mutex. This closes the SIGINT /
    /// SIGTERM spawn→track race: if cleanup starts concurrently, it blocks
    /// until the just-spawned child is present in the tracked set.
    pub fn spawn_tracked(&self, command: &mut Command) -> io::Result<Child> {
        let mut set = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("LSP child registry mutex poisoned"))?;
        let child = command.spawn()?;
        set.insert(child.id());
        Ok(child)
    }

    /// Forget a PID (called when the client is dropped or shut down gracefully).
    pub fn untrack(&self, pid: u32) {
        if let Ok(mut set) = self.inner.lock() {
            set.remove(&pid);
        }
    }

    /// Snapshot of currently-tracked PIDs.
    pub fn pids(&self) -> Vec<u32> {
        self.inner
            .lock()
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Force-kill every tracked child synchronously. Used by the signal
    /// handler to prevent orphaned LSP processes when aft is SIGTERM'd.
    /// Returns the number of process groups that were sent SIGKILL.
    ///
    /// On Unix, kills the entire process group (via `killpg`) rather than
    /// just the wrapper PID. Necessary because npm-wrapped LSP servers like
    /// biome ship as `node biome lsp-proxy` shims that spawn the real
    /// `cli-darwin-arm64 biome lsp-proxy` as a child; killing only the
    /// wrapper leaves the real server orphaned to PID 1.
    ///
    /// `LspClient::spawn` puts each child in its own session via `setsid()`
    /// so `pgid == child.id()`.
    #[cfg(unix)]
    pub fn kill_all(&self) -> usize {
        use std::os::raw::c_int;
        let pids = self.pids();
        let mut killed = 0;
        for pid in pids {
            // SIGKILL = 9. We use the raw libc call rather than crossbeam
            // because we're inside a signal-handler context where allocator
            // and channel use is risky.
            // SAFETY: killpg(2) is async-signal-safe.
            unsafe {
                let pgid = pid as libc::pid_t;
                let rc = libc::killpg(pgid, 9 as c_int);
                if rc == 0 {
                    killed += 1;
                }
            }
        }
        killed
    }

    /// Windows fallback: best-effort kill via `taskkill /F /T`. The `/T`
    /// flag kills the entire process tree (Windows analogue of process
    /// groups). Not technically async-signal-safe but Windows doesn't
    /// deliver signals the same way.
    #[cfg(not(unix))]
    pub fn kill_all(&self) -> usize {
        let pids = self.pids();
        let mut killed = 0;
        for pid in pids {
            if std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .status()
                .is_ok()
            {
                killed += 1;
            }
        }
        killed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_untrack_pids_round_trip() {
        let reg = LspChildRegistry::new();
        reg.track(100);
        reg.track(200);
        let mut pids = reg.pids();
        pids.sort();
        assert_eq!(pids, vec![100, 200]);
        reg.untrack(100);
        assert_eq!(reg.pids(), vec![200]);
    }

    #[test]
    fn clones_share_state() {
        let a = LspChildRegistry::new();
        let b = a.clone();
        a.track(42);
        assert_eq!(b.pids(), vec![42]);
        b.untrack(42);
        assert!(a.pids().is_empty());
    }

    #[test]
    fn untracking_unknown_pid_is_safe() {
        let reg = LspChildRegistry::new();
        reg.untrack(999); // no-op, no panic
        assert!(reg.pids().is_empty());
    }

    #[test]
    fn kill_all_with_no_pids_returns_zero() {
        let reg = LspChildRegistry::new();
        assert_eq!(reg.kill_all(), 0);
    }

    #[test]
    fn spawn_tracked_records_pid_before_returning() {
        let reg = LspChildRegistry::new();
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", "exit", "0"]);
            command
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };

        let mut child = reg.spawn_tracked(&mut command).expect("spawn tracked");
        let pid = child.id();
        assert!(reg.pids().contains(&pid));
        let _ = child.wait();
        reg.untrack(pid);
    }

    // Regression for the npm-wrapper orphan bug: biome ships as `node
    // biome lsp-proxy` (the wrapper) that spawns
    // `cli-darwin-arm64 biome lsp-proxy` (the actual server) as a child.
    // Killing just the wrapper PID via `kill(2)` leaves the real server
    // orphaned to PID 1. `killpg(2)` kills the whole group.
    //
    // We fork a wrapper that does setsid() then forks a grandchild that
    // exec's sleep 120. The grandchild inherits the wrapper's session and
    // PG — matching the real npm-wrapper pattern. Raw fork() avoids
    // Rust's Command::spawn() exec-sync-pipe hang.
    //
    // After killpg(), the grandchild may linger as a zombie (state 'Z' in
    // /proc) because Docker containers often lack a proper init to reap
    // orphans. We verify killpg() reached the grandchild by checking that
    // it is NOT in state 'S' (sleeping/running) — 'Z' or absent both
    // confirm the SIGKILL was delivered.
    #[cfg(unix)]
    #[test]
    fn kill_all_kills_process_group_not_just_wrapper_pid() {
        use std::thread;
        use std::time::{Duration, Instant};

        let test_timeout = Duration::from_secs(10);
        let start = Instant::now();

        let temp = tempfile::tempdir().expect("create temp dir");
        let sync_path = temp.path().join("gc_pid");

        let wrapper_pid_i32: i32 = unsafe { libc::fork() };
        assert_ne!(wrapper_pid_i32, -1, "fork failed for wrapper");

        if wrapper_pid_i32 == 0 {
            unsafe {
                libc::setsid();
                let gc_pid = libc::fork();
                assert_ne!(gc_pid, -1, "fork failed for grandchild");

                if gc_pid == 0 {
                    let cmd = c"sleep";
                    let args: [*const libc::c_char; 3] =
                        [cmd.as_ptr(), c"120".as_ptr(), std::ptr::null()];
                    libc::execvp(cmd.as_ptr(), args.as_ptr());
                    libc::_exit(127);
                }

                let pid_str = format!("{}", gc_pid);
                let path_cstr =
                    std::ffi::CString::new(sync_path.to_str().unwrap().as_bytes()).unwrap();
                let fd = libc::open(
                    path_cstr.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o644,
                );
                assert!(fd >= 0, "open sync file failed");
                libc::write(fd, pid_str.as_ptr() as *const libc::c_void, pid_str.len());
                libc::close(fd);

                let mut status = 0i32;
                libc::waitpid(gc_pid, &mut status, 0);
                libc::_exit(0);
            }
        }

        // Wait for sync file with timeout.
        let grandchild_pid: u32 = loop {
            if start.elapsed() > test_timeout {
                panic!("timeout waiting for grandchild PID sync file");
            }
            if let Ok(s) = std::fs::read_to_string(&sync_path) {
                if let Ok(pid) = s.trim().parse::<u32>() {
                    if pid > 0 {
                        break pid;
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        };

        thread::sleep(Duration::from_millis(200));

        assert!(
            crate::bash_background::process::is_process_alive(wrapper_pid_i32 as u32),
            "wrapper should be alive before kill"
        );
        assert!(
            crate::bash_background::process::is_process_alive(grandchild_pid),
            "grandchild should be alive before kill"
        );

        // Kill the process group via killpg().
        let reg = LspChildRegistry::new();
        reg.track(wrapper_pid_i32 as u32);
        let killed = reg.kill_all();
        assert_eq!(killed, 1, "should report 1 group killed");

        // Reap the wrapper.
        let mut reap_status = 0i32;
        unsafe {
            libc::waitpid(wrapper_pid_i32, &mut reap_status, 0);
        }

        // The wrapper must be dead — that's the core regression.
        thread::sleep(Duration::from_millis(100));
        assert!(
            !crate::bash_background::process::is_process_alive(wrapper_pid_i32 as u32),
            "wrapper must be dead after killpg"
        );

        // For the grandchild: poll up to 3 seconds. After killpg, the
        // grandchild is either:
        //   - Gone (reaped by init) → not alive → PASS
        //   - Zombie (state 'Z' in /proc) → alive but killed → PASS
        //   - Still sleeping (state 'S') → killpg missed it → FAIL
        //
        // is_process_alive() returns true for both running AND zombie
        // processes (kill(pid,0) succeeds for zombies). We disambiguate
        // by reading /proc/<pid>/stat when available.
        let gc_i32 = grandchild_pid as i32;
        let grandchild_dead_or_zombie = loop {
            if start.elapsed() > test_timeout {
                break false;
            }
            if !crate::bash_background::process::is_process_alive(grandchild_pid) {
                break true; // Fully reaped — definitely dead.
            }
            match std::fs::read_to_string(format!("/proc/{}/stat", gc_i32)) {
                Ok(stat) => {
                    // Format: "pid (comm) state ..."
                    // After splitting on ')', the remainder starts with a
                    // space before the state character (e.g. " S ...").
                    if let Some(rest) = stat.split(')').nth(1) {
                        // Skip the leading space to reach the state byte.
                        let state = rest.as_bytes().get(1);
                        if state != Some(&b'S') && state != Some(&b'R') {
                            // Z (zombie), T (stopped), X (dead) — all
                            // mean killpg reached it.
                            break true;
                        }
                    }
                }
                Err(_) => {
                    // /proc entry gone — process fully reaped.
                    break true;
                }
            }
            thread::sleep(Duration::from_millis(50));
        };

        assert!(
            grandchild_dead_or_zombie,
            "grandchild must be dead or zombie after killpg — \
             still running means killpg missed it (npm-wrapper orphan bug)"
        );
    }
}
