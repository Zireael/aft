// Test-only syscall tracer for the durability census (see
// durability_census_test.rs). Built at test time into a dylib and loaded into
// the `aft` child with DYLD_INSERT_LIBRARIES, so it observes the real store
// code without any change to it. macOS only.
//
// Every traced call appends one tab-separated line to the file named by
// AFT_SYNC_TRACE:
//   pid  tid  start_ns  op  kind  duration_ns  bytes  result  path  path2
// `kind` is `dir` or `file` for the descriptor being synced or written. Only
// regular files are traced for writes (pipes, sockets and ttys are skipped).
// Calls made from inside this library are not interposed by dyld, so writing
// the trace never recurses.

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

extern int fdatasync(int);
extern int renamex_np(const char *, const char *, unsigned int);
extern int renameatx_np(int, const char *, int, const char *, unsigned int);

static int trace_fd = -1;
static pthread_once_t trace_once = PTHREAD_ONCE_INIT;

static void trace_open(void) {
    const char *path = getenv("AFT_SYNC_TRACE");
    if (path != NULL && path[0] != '\0') {
        trace_fd = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    }
}

static uint64_t now_ns(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }

static const char *fd_kind(int fd, int *regular) {
    struct stat st;
    *regular = 0;
    if (fstat(fd, &st) != 0) return "unknown";
    if (S_ISDIR(st.st_mode)) return "dir";
    if (S_ISREG(st.st_mode)) {
        *regular = 1;
        return "file";
    }
    return "other";
}

static void fd_path(int fd, char *out) {
    if (fcntl(fd, F_GETPATH, out) != 0) snprintf(out, MAXPATHLEN, "<fd %d>", fd);
}

static void at_path(int dirfd, const char *name, char *out) {
    if (name == NULL) {
        snprintf(out, MAXPATHLEN, "<null>");
        return;
    }
    if (name[0] == '/' || dirfd == AT_FDCWD) {
        snprintf(out, MAXPATHLEN, "%s", name);
        return;
    }
    char dir[MAXPATHLEN];
    fd_path(dirfd, dir);
    snprintf(out, MAXPATHLEN, "%s/%s", dir, name);
}

static void emit(const char *op, const char *kind, uint64_t start, uint64_t duration,
                 long long bytes, int result, const char *path, const char *path2) {
    pthread_once(&trace_once, trace_open);
    if (trace_fd < 0) return;
    uint64_t tid = 0;
    pthread_threadid_np(NULL, &tid);
    char line[3 * MAXPATHLEN];
    int len = snprintf(line, sizeof line, "%d\t%llu\t%llu\t%s\t%s\t%llu\t%lld\t%d\t%s\t%s\n",
                       getpid(), (unsigned long long)tid, (unsigned long long)start, op, kind,
                       (unsigned long long)duration, bytes, result, path ? path : "",
                       path2 ? path2 : "");
    if (len > 0) {
        if ((size_t)len > sizeof line) len = sizeof line;
        (void)write(trace_fd, line, (size_t)len);
    }
}

static void emit_fd(const char *op, int fd, uint64_t start, uint64_t duration, long long bytes,
                    int result) {
    if (fd == trace_fd) return;
    int regular;
    const char *kind = fd_kind(fd, &regular);
    char path[MAXPATHLEN];
    fd_path(fd, path);
    emit(op, kind, start, duration, bytes, result, path, NULL);
}

static int traced_fcntl(int fd, int cmd, ...) {
    va_list args;
    va_start(args, cmd);
    void *arg = va_arg(args, void *);
    va_end(args);
    if (cmd != F_FULLFSYNC && cmd != F_BARRIERFSYNC) return fcntl(fd, cmd, arg);
    int saved;
    uint64_t start = now_ns();
    int result = fcntl(fd, cmd, arg);
    uint64_t duration = now_ns() - start;
    saved = errno;
    emit_fd(cmd == F_FULLFSYNC ? "F_FULLFSYNC" : "F_BARRIERFSYNC", fd, start, duration, 0, result);
    errno = saved;
    return result;
}

static int traced_fsync(int fd) {
    uint64_t start = now_ns();
    int result = fsync(fd);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_fd("fsync", fd, start, duration, 0, result);
    errno = saved;
    return result;
}

static int traced_fdatasync(int fd) {
    uint64_t start = now_ns();
    int result = fdatasync(fd);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_fd("fdatasync", fd, start, duration, 0, result);
    errno = saved;
    return result;
}

static void emit_write(const char *op, int fd, uint64_t start, uint64_t duration, ssize_t result) {
    if (fd == trace_fd || result < 0) return;
    int regular;
    (void)fd_kind(fd, &regular);
    if (!regular) return;
    char path[MAXPATHLEN];
    fd_path(fd, path);
    emit(op, "file", start, duration, (long long)result, 0, path, NULL);
}

static ssize_t traced_write(int fd, const void *buffer, size_t size) {
    uint64_t start = now_ns();
    ssize_t result = write(fd, buffer, size);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_write("write", fd, start, duration, result);
    errno = saved;
    return result;
}

static ssize_t traced_pwrite(int fd, const void *buffer, size_t size, off_t offset) {
    uint64_t start = now_ns();
    ssize_t result = pwrite(fd, buffer, size, offset);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_write("pwrite", fd, start, duration, result);
    errno = saved;
    return result;
}

static ssize_t traced_writev(int fd, const struct iovec *iov, int count) {
    uint64_t start = now_ns();
    ssize_t result = writev(fd, iov, count);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_write("writev", fd, start, duration, result);
    errno = saved;
    return result;
}

static void emit_rename(const char *op, uint64_t start, uint64_t duration, int result,
                        const char *from, const char *to) {
    emit(op, "entry", start, duration, 0, result, from, to);
}

static int traced_rename(const char *from, const char *to) {
    uint64_t start = now_ns();
    int result = rename(from, to);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("rename", start, duration, result, from, to);
    errno = saved;
    return result;
}

static int traced_renamex_np(const char *from, const char *to, unsigned int flags) {
    uint64_t start = now_ns();
    int result = renamex_np(from, to, flags);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("rename", start, duration, result, from, to);
    errno = saved;
    return result;
}

static int traced_renameat(int from_dir, const char *from, int to_dir, const char *to) {
    char from_path[MAXPATHLEN], to_path[MAXPATHLEN];
    at_path(from_dir, from, from_path);
    at_path(to_dir, to, to_path);
    uint64_t start = now_ns();
    int result = renameat(from_dir, from, to_dir, to);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("rename", start, duration, result, from_path, to_path);
    errno = saved;
    return result;
}

static int traced_renameatx_np(int from_dir, const char *from, int to_dir, const char *to,
                               unsigned int flags) {
    char from_path[MAXPATHLEN], to_path[MAXPATHLEN];
    at_path(from_dir, from, from_path);
    at_path(to_dir, to, to_path);
    uint64_t start = now_ns();
    int result = renameatx_np(from_dir, from, to_dir, to, flags);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("rename", start, duration, result, from_path, to_path);
    errno = saved;
    return result;
}

static int traced_link(const char *from, const char *to) {
    uint64_t start = now_ns();
    int result = link(from, to);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("link", start, duration, result, from, to);
    errno = saved;
    return result;
}

static int traced_linkat(int from_dir, const char *from, int to_dir, const char *to, int flags) {
    char from_path[MAXPATHLEN], to_path[MAXPATHLEN];
    at_path(from_dir, from, from_path);
    at_path(to_dir, to, to_path);
    uint64_t start = now_ns();
    int result = linkat(from_dir, from, to_dir, to, flags);
    uint64_t duration = now_ns() - start;
    int saved = errno;
    emit_rename("link", start, duration, result, from_path, to_path);
    errno = saved;
    return result;
}

#define INTERPOSE(replacement, original)                                                 \
    __attribute__((used)) static struct {                                               \
        const void *replacement_fn;                                                     \
        const void *original_fn;                                                        \
    } interpose_##original __attribute__((section("__DATA,__interpose"))) = {           \
        (const void *)(unsigned long)&replacement, (const void *)(unsigned long)&original \
    };

INTERPOSE(traced_fcntl, fcntl)
INTERPOSE(traced_fsync, fsync)
INTERPOSE(traced_fdatasync, fdatasync)
INTERPOSE(traced_write, write)
INTERPOSE(traced_pwrite, pwrite)
INTERPOSE(traced_writev, writev)
INTERPOSE(traced_rename, rename)
INTERPOSE(traced_renamex_np, renamex_np)
INTERPOSE(traced_renameat, renameat)
INTERPOSE(traced_renameatx_np, renameatx_np)
INTERPOSE(traced_link, link)
INTERPOSE(traced_linkat, linkat)
