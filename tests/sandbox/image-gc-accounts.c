/* Exercise libc's account-file consumers against the isolated store, not the host /etc. */
#include <grp.h>
#include <pwd.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
  if (argc != 3)
    return 1;
  FILE *passwd = fopen(argv[1], "r");
  FILE *group = fopen(argv[2], "r");
  if (passwd == NULL || group == NULL)
    return 2;
  int root_user = 0, sandbox_user = 0, root_group = 0, sandbox_group = 0;
  struct passwd *user;
  while ((user = fgetpwent(passwd)) != NULL) {
    root_user |= user->pw_uid == 0 && user->pw_gid == 0 && strcmp(user->pw_name, "root") == 0;
    sandbox_user |= user->pw_uid == 1000 && user->pw_gid == 1000 && strcmp(user->pw_name, "wrix") == 0;
  }
  struct group *entry;
  while ((entry = fgetgrent(group)) != NULL) {
    root_group |= entry->gr_gid == 0 && strcmp(entry->gr_name, "root") == 0;
    sandbox_group |= entry->gr_gid == 1000 && strcmp(entry->gr_name, "wrix") == 0;
  }
  fclose(passwd);
  fclose(group);
  return root_user && sandbox_user && root_group && sandbox_group ? 0 : 3;
}
