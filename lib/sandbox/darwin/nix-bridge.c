/* Fixed worker-local SSH transport. No destinations or executable names are accepted. */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <linux/capability.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#define LIMIT 16
#define CHUNK 16384
#define DIRECTORY "/run/wrix/apple-nix-bridge"
#define SOCKET DIRECTORY "/socket"
static const char magic[] = "WRIXNB01";
struct stream { int fd; uint32_t id; size_t used; int credit, ack; unsigned char pending[CHUNK]; };
static int nonblock(int fd) { return fcntl(fd, F_SETFL, O_NONBLOCK); }
static int exact(int fd, void *bytes, size_t count, int writing) {
    unsigned char *p = bytes;
    while (count) {
        ssize_t n = writing ? write(fd, p, count) : read(fd, p, count);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return -1;
        p += n; count -= (size_t)n;
    }
    return 0;
}
static int address(int fd, int server) {
    struct sockaddr_un addr = { .sun_family = AF_UNIX };
    memcpy(addr.sun_path, SOCKET, sizeof(SOCKET));
    return server ? bind(fd, (struct sockaddr *)&addr, sizeof(addr)) : connect(fd, (struct sockaddr *)&addr, sizeof(addr));
}
static int connect_stdio(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0 || address(fd, 0)) return 1;
    unsigned char in[CHUNK], out[CHUNK]; size_t ni = 0, no = 0;
    int eof = 0;
    if (nonblock(fd) || nonblock(0) || nonblock(1)) return 1;
    for (;;) {
        struct pollfd p[3] = {{0, !eof && !ni ? POLLIN : 0, 0}, {1, no ? POLLOUT : 0, 0}, {fd, (!no ? POLLIN : 0) | (ni ? POLLOUT : 0), 0}};
        if (poll(p, 3, -1) < 0) { if (errno == EINTR) continue; return 1; }
        if (!eof && !ni && (p[0].revents & (POLLIN | POLLHUP))) {
            ssize_t n = read(0, in, sizeof(in));
            if (n == 0) eof = 1; else if (n > 0) ni = (size_t)n; else if (errno != EAGAIN) return 1;
        }
        if (p[2].revents & POLLOUT) {
            ssize_t n = write(fd, in, ni);
            if (n > 0) { memmove(in, in+n, ni-(size_t)n); ni -= (size_t)n; } else if (errno != EAGAIN) return 1;
        }
        if (eof == 1 && !ni) { shutdown(fd, SHUT_WR); eof = 2; }
        if (!no && (p[2].revents & (POLLIN | POLLHUP))) {
            ssize_t n = read(fd, out, sizeof(out));
            if (n == 0) { if (!no) return 0; } else if (n > 0) no = (size_t)n; else if (errno != EAGAIN) return 1;
        }
        if (p[1].revents & POLLOUT) {
            ssize_t n = write(1, out, no);
            if (n > 0) { memmove(out, out+n, no-(size_t)n); no -= (size_t)n; } else if (errno != EAGAIN) return 1;
        }
        if ((p[0].revents | p[1].revents | p[2].revents) & (POLLERR | POLLNVAL)) return 1;
    }
}
static size_t frame(unsigned char *out, unsigned char tag, uint32_t id, const void *data, size_t len) {
    uint32_t wire = htonl(id), length = htonl((uint32_t)len);
    out[0] = tag; memcpy(out+1, &wire, 4); memcpy(out+5, &length, 4);
    if (len && data != out+9) memcpy(out+9, data, len);
    return len+9;
}
static int serve(void) {
    umask(0077);
    if (mkdir(DIRECTORY, 0700)) return 1;
    int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (listener < 0 || address(listener, 1) || listen(listener, LIMIT)) return 1;
    if (exact(1, (void *)magic, 8, 1)) return 1;
    char peer[8];
    if (exact(0, peer, 8, 0) || memcmp(peer, magic, 8)) return 1;
    if (nonblock(0) || nonblock(1) || nonblock(listener)) return 1;
    struct stream slots[LIMIT];
    for (int i = 0; i < LIMIT; ++i) slots[i] = (struct stream){ .fd = -1 };
    unsigned char incoming[CHUNK+9], outgoing[CHUNK+9];
    size_t have = 0, need = 9, sending = 0; uint32_t serial = 0;
    for (;;) {
        struct pollfd p[LIMIT+3] = {{0, POLLIN, 0}, {1, sending ? POLLOUT : 0, 0}, {listener, !sending ? POLLIN : 0, 0}};
        for (int i = 0; i < LIMIT; ++i) p[i+3] = (struct pollfd){slots[i].fd, (!sending && slots[i].credit ? POLLIN : 0) | (slots[i].used ? POLLOUT : 0), 0};
        if (poll(p, LIMIT+3, -1) < 0) { if (errno == EINTR) continue; return 1; }
        if (p[1].revents & POLLOUT) {
            ssize_t n = write(1, outgoing, sending);
            if (n > 0) { memmove(outgoing, outgoing+n, sending-(size_t)n); sending -= (size_t)n; } else if (errno != EAGAIN) return 1;
        }
        if (!sending && (p[2].revents & POLLIN)) {
            int fd = accept4(listener, NULL, NULL, SOCK_NONBLOCK | SOCK_CLOEXEC), slot = -1;
            if (fd < 0) return 1;
            for (int i = 0; i < LIMIT; ++i) if (slots[i].fd < 0) { slot = i; break; }
            if (slot < 0) close(fd);
            else {
                if (++serial == 0) return 1;
                slots[slot] = (struct stream){.fd = fd, .id = serial, .credit = 1};
                sending = frame(outgoing, 1, serial, NULL, 0);
            }
        }
        if (p[0].revents & (POLLIN | POLLHUP)) {
            ssize_t n = read(0, incoming+have, need-have);
            if (n == 0) return 1;
            if (n < 0 && errno != EAGAIN) return 1;
            if (n > 0) have += (size_t)n;
            if (have >= 9) {
                uint32_t id, len; memcpy(&id, incoming+1, 4); memcpy(&len, incoming+5, 4); id = ntohl(id); len = ntohl(len);
                if (!id || id > serial || len > CHUNK || (incoming[0] != 2 && incoming[0] != 3 && incoming[0] != 4) || (incoming[0] != 2 && len) || (incoming[0] == 2 && !len)) return 1;
                need = len+9;
                if (have == need) {
                    struct stream *s = NULL;
                    for (int i = 0; i < LIMIT; ++i) if (slots[i].id == id && slots[i].fd >= 0) s = &slots[i];
                    if (s) {
                        if (incoming[0] == 3) { close(s->fd); s->fd = -1; s->used = 0; }
                        else if (incoming[0] == 4) { if (s->credit) return 1; s->credit = 1; }
                        else if (s->used || s->ack) return 1;
                        else { memcpy(s->pending, incoming+9, len); s->used = len; }
                    }
                    have = 0; need = 9;
                }
            }
        }
        for (int i = 0; i < LIMIT; ++i) {
            struct stream *s = &slots[i];
            if (s->fd < 0) continue;
            if (p[i+3].revents & POLLOUT) {
                ssize_t n = write(s->fd, s->pending, s->used);
                if (n > 0) { memmove(s->pending, s->pending+n, s->used-(size_t)n); s->used -= (size_t)n; if (!s->used) s->ack = 1; }
                else if (errno != EAGAIN) return 1;
            }
            if (!sending && s->ack) { sending = frame(outgoing, 4, s->id, NULL, 0); s->ack = 0; }
            if (!sending && s->credit && (p[i+3].revents & (POLLIN | POLLHUP | POLLERR))) {
                ssize_t n = read(s->fd, outgoing+9, CHUNK);
                if (n > 0) { sending = frame(outgoing, 2, s->id, outgoing+9, (size_t)n); s->credit = 0; }
                else if (n == 0 || errno != EAGAIN) {
                    sending = frame(outgoing, 3, s->id, NULL, 0); close(s->fd); s->fd = -1; s->used = 0;
                }
            }
        }
        if ((p[0].revents | p[1].revents) & (POLLERR | POLLNVAL)) return 1;
    }
}
int main(int argc, char **argv) {
    struct __user_cap_header_struct header = { .version = _LINUX_CAPABILITY_VERSION_3, .pid = 0 };
    struct __user_cap_data_struct capabilities[2] = {{0}, {0}};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) || syscall(SYS_capset, &header, capabilities)) return 1;
    if (argc != 2) return 2;
    if (!strcmp(argv[1], "serve")) return serve();
    if (!strcmp(argv[1], "connect")) return connect_stdio();
    return 2;
}
