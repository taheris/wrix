/* Model Linux's pipe-user-pages-soft boundary without changing kernel limits. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int
wrix_test_pipe2 (int fds[2], int flags)
{
  long page = sysconf (_SC_PAGESIZE);
  if (page <= 0)
    return -1;
  if (pipe2 (fds, flags) < 0)
    return -1;
  if (fcntl (fds[0], F_SETPIPE_SZ, (int) (2 * page)) < 0)
    {
      int saved_errno = errno;
      close (fds[0]);
      close (fds[1]);
      errno = saved_errno;
      return -1;
    }
  return 0;
}

int
wrix_test_fcntl (int fd, int command, ...)
{
  if (command == F_GETPIPE_SZ)
    return fcntl (fd, command);
  if (command == F_SETPIPE_SZ)
    {
      va_list args;
      va_start (args, command);
      int size = va_arg (args, int);
      va_end (args);
      int capacity = fcntl (fd, F_GETPIPE_SZ);
      if (capacity < 0)
        return -1;
      if (size > capacity)
        {
          errno = EPERM;
          return -1;
        }
      return fcntl (fd, command, size);
    }
  errno = EINVAL;
  return -1;
}

#ifdef WRIX_PIPE_FIXTURE_SELF_TEST
static int
test_two_page_nonblocking_pipe_round_trips_data (void)
{
  int fds[2];
  if (wrix_test_pipe2 (fds, O_NONBLOCK) < 0)
    return 1;
  long page = sysconf (_SC_PAGESIZE);
  char result[4];
  int failed = page <= 0
               || wrix_test_fcntl (fds[0], F_GETPIPE_SZ) != 2 * page
               || fcntl (fds[0], F_GETPIPE_SZ) != 2 * page
               || !(fcntl (fds[0], F_GETFL) & O_NONBLOCK)
               || write (fds[1], "pipe", 4) != 4
               || read (fds[0], result, 4) != 4
               || memcmp (result, "pipe", 4) != 0;
  if (close (fds[0]) < 0 || close (fds[1]) < 0)
    return 1;
  return failed;
}

static int
test_growth_denied_but_shrinking_allowed (void)
{
  int fds[2];
  if (wrix_test_pipe2 (fds, O_NONBLOCK) < 0)
    return 1;
  int capacity = fcntl (fds[0], F_GETPIPE_SZ);
  long page = sysconf (_SC_PAGESIZE);
  int failed = capacity <= 0 || page <= 0
               || wrix_test_fcntl (fds[0], F_SETPIPE_SZ, capacity + 1) != -1
               || errno != EPERM
               || fcntl (fds[0], F_GETPIPE_SZ) != capacity
               || wrix_test_fcntl (fds[0], F_SETPIPE_SZ, (int) page) != page
               || fcntl (fds[0], F_GETPIPE_SZ) != page;
  if (close (fds[0]) < 0 || close (fds[1]) < 0)
    return 1;
  return failed;
}

int
main (void)
{
  if (test_two_page_nonblocking_pipe_round_trips_data ()
      || test_growth_denied_but_shrinking_allowed ())
    {
      fprintf (stderr, "constrained-pipe fixture conformance failed\n");
      return 1;
    }
  return 0;
}
#endif
