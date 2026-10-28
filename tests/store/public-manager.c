/* Qualification-only fixed systemd job in a freshly owned delegated cgroup. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <limits.h>
#include <linux/magic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/vfs.h>
#include <unistd.h>
int main(int argc, char **argv) {
    char canonical[PATH_MAX], pid[32];
    struct stat info;
    struct statfs filesystem;
    if (argc != 2 || getuid() == 0 || !realpath(argv[1], canonical) ||
        strcmp(canonical, argv[1]) || strncmp(canonical, "/sys/fs/cgroup/", 15)) return 64;
    const char *name = strrchr(canonical, '/') + 1;
    if (strncmp(name, "wx-doaia-public-", 16) || !strstr(name, ".scope")) return 64;
    int directory = open(canonical, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (directory < 0 || fstat(directory, &info) || info.st_uid != getuid() ||
        fstatfs(directory, &filesystem) || filesystem.f_type != CGROUP2_SUPER_MAGIC) return 65;
    int membership = openat(directory, "cgroup.procs", O_WRONLY | O_NOFOLLOW | O_CLOEXEC);
    int size = snprintf(pid, sizeof(pid), "%ld\n", (long)getpid());
    if (membership < 0 || write(membership, pid, size) != size) return 66;
    close(membership); close(directory);
    char *const arguments[] = { "systemd", "--user", "--unit=basic.target", NULL };
    execv(WRIX_SYSTEMD, arguments);
    perror("fixed private systemd job"); return 127;
}
