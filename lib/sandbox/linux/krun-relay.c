/*
 * krun-relay — PTY relay for krun microVM entrypoint
 *
 * krun's virtio console (/dev/console) doesn't support changing terminal
 * attributes (raw mode, echo, etc.) reliably. This program creates a real
 * PTY where raw mode works, and relays I/O between the console and the PTY.
 *
 * Key fix: the console's ICRNL flag converts CR (Enter, 0x0d) to LF (0x0a).
 * Claude Code expects CR for "submit." The relay converts LF back to CR on
 * stdin before writing to the PTY master.
 *
 * If the console does support raw mode (tcsetattr succeeds), the relay sets
 * it for full keystroke-by-keystroke interactivity. If not, input is
 * line-buffered by the console but Enter still works correctly.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

static volatile sig_atomic_t child_exited = 0;
static volatile int child_status = 0;

static void sigchld_handler(int sig) {
    (void)sig;
    int status;
    if (waitpid(-1, &status, WNOHANG) > 0) {
        child_status = status;
        child_exited = 1;
    }
}

int main(int argc, char **argv) {
    /* Terminal size from env (set by launcher) */
    int rows = 24, cols = 80;
    const char *r = getenv("WRIX_TERM_ROWS");
    const char *c = getenv("WRIX_TERM_COLS");
    if (r) rows = atoi(r);
    if (c) cols = atoi(c);

    /* Set up SIGCHLD handler before fork */
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigchld_handler;
    sa.sa_flags = SA_NOCLDSTOP;
    sigaction(SIGCHLD, &sa, NULL);

    /* Create PTY pair and fork */
    int master;
    struct winsize ws = {
        .ws_row = (unsigned short)rows,
        .ws_col = (unsigned short)cols
    };
    pid_t pid = forkpty(&master, NULL, NULL, &ws);
    if (pid < 0) {
        perror("forkpty");
        return 1;
    }

    if (pid == 0) {
        /* Child: runs inside the real PTY */
        /* Command to exec: argv[1..] or default to /krun-init.sh */
        if (argc > 1) {
            execvp(argv[1], argv + 1);
        } else {
            char *args[] = {"/krun-init.sh", NULL};
            execv(args[0], args);
        }
        perror("exec");
        _exit(127);
    }

    /* Parent: relay I/O between stdin/stdout and PTY master */

    /* Try to set stdin (console) to raw mode. If this works, keystrokes
     * arrive individually for full interactivity. If not, input is
     * line-buffered but the \n→\r conversion still fixes Enter. */
    struct termios orig_termios;
    int have_orig = (tcgetattr(STDIN_FILENO, &orig_termios) == 0);
    if (have_orig) {
        struct termios raw = orig_termios;
        cfmakeraw(&raw);
        tcsetattr(STDIN_FILENO, TCSANOW, &raw);
    }

    /* Make master fd non-blocking for cleaner poll loop */
    int flags = fcntl(master, F_GETFL);
    if (flags >= 0)
        fcntl(master, F_SETFL, flags | O_NONBLOCK);

    /* Relay loop */
    unsigned char buf[4096];

    while (!child_exited) {
        struct pollfd fds[2];
        fds[0].fd = STDIN_FILENO;
        fds[0].events = POLLIN;
        fds[1].fd = master;
        fds[1].events = POLLIN;

        int ret = poll(fds, 2, 200);  /* 200ms timeout to check child_exited */
        if (ret < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (ret == 0) continue;

        /* stdin → PTY master (with \n → \r conversion) */
        if (fds[0].revents & POLLIN) {
            ssize_t n = read(STDIN_FILENO, buf, sizeof(buf));
            if (n <= 0) break;
            /* Console ICRNL converts CR (Enter) to LF. Convert back so
             * Claude Code's TUI sees CR for submit. */
            for (ssize_t i = 0; i < n; i++) {
                if (buf[i] == '\n')
                    buf[i] = '\r';
            }
            write(master, buf, (size_t)n);
        }

        /* PTY master → stdout */
        if (fds[1].revents & POLLIN) {
            ssize_t n = read(master, buf, sizeof(buf));
            if (n > 0)
                write(STDOUT_FILENO, buf, (size_t)n);
            else if (n < 0 && errno != EAGAIN && errno != EIO)
                break;
        }

        if (fds[0].revents & (POLLHUP | POLLERR)) break;
        if (fds[1].revents & POLLHUP) break;
        /* POLLERR on master is normal when child exits */
    }

    /* Drain any remaining output from PTY */
    for (;;) {
        ssize_t n = read(master, buf, sizeof(buf));
        if (n <= 0) break;
        write(STDOUT_FILENO, buf, (size_t)n);
    }

    close(master);

    /* Restore console terminal settings */
    if (have_orig)
        tcsetattr(STDIN_FILENO, TCSANOW, &orig_termios);

    /* Reap child if not yet reaped */
    if (!child_exited) {
        int status;
        waitpid(pid, &status, 0);
        child_status = status;
    }

    return WIFEXITED(child_status) ? WEXITSTATUS(child_status) : 1;
}
