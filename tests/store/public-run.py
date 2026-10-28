"""A real PTY for the fixed packaged public Run qualification command."""
import errno
import os
import pty
import sys

assert len(sys.argv) == 2
profile = os.path.realpath(sys.argv[1])
assert os.path.isfile(profile)
pid, master = pty.fork()
if pid == 0:
    os.execv('@cli@', ['@cli@', '--profile-config', profile, 'run',
                       '--nix-store', 'shared', '--no-cache', '--', 'public-second'])
try:
    while True:
        try:
            data = os.read(master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        if not data:
            break
        os.write(1, data)
finally:
    os.close(master)
_, status = os.waitpid(pid, 0)
raise SystemExit(os.waitstatus_to_exitcode(status))
