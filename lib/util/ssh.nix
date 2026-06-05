# SSH credential paths and runtime setup for sandbox containers.
#
# Centralises container-side mount targets so Linux and Darwin launchers
# all use the same key locations.  The runtime setup script (gitSshSetup)
# is sourced by entrypoints to export GIT_SSH_COMMAND, write ~/.ssh/config,
# and configure git signing.
{
  # Container-side key directory.
  # NOT under ~/.ssh/ — on Linux, podman auto-creates parent dirs for
  # bind mounts owned by root, which blocks chmod in the entrypoint.
  containerKeyDir = "/etc/wrix/keys";

  # System-wide SSH known_hosts target.
  # OpenSSH reads /etc/ssh/ssh_known_hosts automatically.
  knownHostsTarget = "/etc/ssh/ssh_known_hosts";

  # Known_hosts source directory target for platforms that must mount
  # directories instead of files (Darwin VirtioFS).
  knownHostsDirTarget = "/etc/wrix/known_hosts_dir";

  # Runtime git/SSH setup script — sourced (not executed) by entrypoints.
  # Kept as a .sh file so shellcheck works.
  gitSshSetup = ./git-ssh-setup.sh;
}
