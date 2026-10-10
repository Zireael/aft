//! Exit observation for tasks adopted without a child handle after a restart.
//! A kernel process handle avoids repeatedly resolving child-writable artifacts.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct AdoptedExit {
    initialized: bool,
    exited: bool,
    #[cfg(unix)]
    monitor: Option<ProcessExit>,
    fallback: MarkerBackoff,
}

impl AdoptedExit {
    pub(crate) fn marker_due(&mut self, pid: Option<u32>, now: Instant) -> bool {
        if self.exited {
            return true;
        }
        #[cfg(unix)]
        {
            if !self.initialized {
                self.initialized = true;
                self.monitor = pid.and_then(|pid| ProcessExit::open(pid).ok());
            }
            if let Some(monitor) = &self.monitor {
                match monitor.exited() {
                    Ok(exited) => {
                        self.exited = exited;
                        return exited;
                    }
                    Err(_) => self.monitor = None,
                }
            }
        }
        #[cfg(not(unix))]
        let _ = pid;
        self.fallback.due(now)
    }
}

#[derive(Default)]
pub(crate) struct MarkerBackoff {
    next: Option<Instant>,
    interval: Option<Duration>,
}

impl MarkerBackoff {
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        let interval = self.interval.unwrap_or(Duration::from_millis(500));
        self.next = Some(now + interval);
        self.interval = Some((interval * 2).min(Duration::from_secs(5)));
        true
    }
}

#[cfg(unix)]
struct ProcessExit(std::os::fd::OwnedFd);

#[cfg(unix)]
impl ProcessExit {
    #[cfg(target_os = "linux")]
    fn open(pid: u32) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(unsafe {
            std::os::fd::OwnedFd::from_raw_fd(fd as i32)
        }))
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn open(pid: u32) -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let monitor = Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        // kqueue does not set close-on-exec: detached commands must not inherit it.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        event.ident = pid as _;
        event.filter = libc::EVFILT_PROC;
        event.flags = libc::EV_ADD | libc::EV_ONESHOT;
        event.fflags = libc::NOTE_EXIT;
        let result = unsafe {
            libc::kevent(
                monitor.0.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(monitor)
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    fn open(_pid: u32) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no kernel exit observer",
        ))
    }

    #[cfg(target_os = "linux")]
    fn exited(&self) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(std::io::Error::other("invalid pidfd"));
        }
        Ok(poll.revents & (libc::POLLIN | libc::POLLHUP) != 0)
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    fn exited(&self) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd;
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let result = unsafe {
            libc::kevent(
                self.0.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if result > 0 && event.flags & libc::EV_ERROR != 0 {
            return Err(std::io::Error::from_raw_os_error(event.data as i32));
        }
        Ok(result > 0 && event.fflags & libc::NOTE_EXIT != 0)
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    fn exited(&self) -> std::io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_cost_marker_fallback_backs_off_to_five_seconds() {
        let mut backoff = MarkerBackoff::default();
        let now = Instant::now();
        let mut probes = 0;
        for tick in 0..120 {
            probes += usize::from(backoff.due(now + Duration::from_millis(tick * 500)));
        }
        assert_eq!(
            probes, 15,
            "long-run fallback must not resolve artifacts 120 times/minute"
        );
    }

    #[cfg(unix)]
    #[test]
    fn watchdog_cost_kernel_observer_reports_exit_without_marker() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let monitor = ProcessExit::open(child.id()).unwrap();
        assert!(!monitor.exited().unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(monitor.exited().unwrap());
    }
}
