/*
 * libfakeuid.so — LD_PRELOAD library for krun microVM
 *
 * krun maps the host user to root (uid 0) inside the VM, but Claude Code
 * refuses --dangerously-skip-permissions as root. Override getuid/geteuid
 * to report uid 1000 while the kernel retains root credentials for file
 * access. Also patch TIOCGWINSZ as a fallback if the PTY size isn't set.
 *
 * Terminal I/O is handled by krun-relay (PTY relay as PID 1), not here.
 */

#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <unistd.h>

/* UID/GID spoofing */
uid_t getuid(void)  { return 1000; }
uid_t geteuid(void) { return 1000; }
gid_t getgid(void)  { return 1000; }
gid_t getegid(void) { return 1000; }

/* Intercept ioctl for terminal size fallback */
int ioctl(int fd, unsigned long request, ...) {
    va_list ap;
    va_start(ap, request);
    void *arg = va_arg(ap, void *);
    va_end(ap);

    static int (*real_ioctl)(int, unsigned long, ...) = NULL;
    if (!real_ioctl)
        real_ioctl = (int (*)(int, unsigned long, ...))dlsym(RTLD_NEXT, "ioctl");

    int ret = real_ioctl(fd, request, arg);

    /* Patch 0x0 terminal size to host dimensions from env vars */
    if (request == TIOCGWINSZ) {
        struct winsize *ws = (struct winsize *)arg;
        if (ret != 0 || (ws->ws_row == 0 && ws->ws_col == 0)) {
            const char *rows = getenv("WRIX_TERM_ROWS");
            const char *cols = getenv("WRIX_TERM_COLS");
            if (rows) ws->ws_row = (unsigned short)atoi(rows);
            if (cols) ws->ws_col = (unsigned short)atoi(cols);
            ret = 0;
        }
    }

    return ret;
}
