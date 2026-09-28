#include <unistd.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <time.h>
#include <execinfo.h>
// macOS-only syscall byte probe for an isolated process, not a physical-I/O meter.
// Inject only into a process you launch. AFT_IO_PROBE_LOG is rewritten every 2s;
// those diagnostic writes affect the OS process counter but are excluded here.
// F_GETPATH failures (pipes/sockets) and paths beyond 4096 rows are not counted.
// Production binaries are unchanged.
struct row { char path[1024]; unsigned long long bytes, calls, syncs; };
static struct row rows[4096];
static int count;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static void note(int fd, ssize_t bytes, int sync) {
 char path[1024] = "<unknown>";
 if (fcntl(fd, F_GETPATH, path)) return;
 if (strstr(path,"io-probe.tsv")) return;
 pthread_mutex_lock(&lock);
 int i;
 for(i=0;i<count;i++) if(!strcmp(rows[i].path,path)) break;
 int fresh=i==count;
 if(i==count && count<4096) { strcpy(rows[i].path,path); count++; }
 if(i<4096) { if(bytes>0) rows[i].bytes+=bytes; rows[i].calls++; rows[i].syncs+=sync; }
 pthread_mutex_unlock(&lock);
 if(fresh && strstr(path,"etilqs_")) {
  void *frames[32]; int n=backtrace(frames,32);
  dprintf(2,"SQLITE_TEMP_FILE %s first_write=%zd\n",path,bytes);
  backtrace_symbols_fd(frames,n,2);
 }
}
static ssize_t measured_write(int fd,const void *p,size_t n) { ssize_t r=write(fd,p,n); note(fd,r,0); return r; }
static ssize_t measured_pwrite(int fd,const void *p,size_t n,off_t off) { ssize_t r=pwrite(fd,p,n,off); note(fd,r,0); return r; }
static ssize_t measured_writev(int fd,const struct iovec *v,int n) { ssize_t r=writev(fd,v,n); note(fd,r,0); return r; }
static int measured_fsync(int fd) { int r=fsync(fd); note(fd,0,1); return r; }
#define INTERPOSE(replacement,original) __attribute__((used)) static struct { const void *new; const void *old; } pair_##original __attribute__((section("__DATA,__interpose"))) = { (const void*)replacement, (const void*)original };
INTERPOSE(measured_write,write)
INTERPOSE(measured_pwrite,pwrite)
INTERPOSE(measured_writev,writev)
INTERPOSE(measured_fsync,fsync)
static void *report(void *unused) {
 (void)unused;
 const char *path=getenv("AFT_IO_PROBE_LOG");
 if(!path) return NULL;
 for(;;) {
  sleep(2);
  FILE *f=fopen(path,"w"); if(!f) continue;
  pthread_mutex_lock(&lock);
  int n=count;
  struct row *copy=malloc(sizeof(struct row)*n);
  memcpy(copy,rows,sizeof(struct row)*n);
  pthread_mutex_unlock(&lock);
  for(int i=0;i<n;i++) fprintf(f,"%llu\t%llu\t%llu\t%s\n", copy[i].bytes,copy[i].calls,copy[i].syncs,copy[i].path);
  free(copy);
  fclose(f);
 }
 return NULL;
}
__attribute__((constructor)) static void start(void) { pthread_t t; pthread_create(&t,NULL,report,NULL); pthread_detach(t); }
