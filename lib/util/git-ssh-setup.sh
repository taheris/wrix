# shellcheck shell=bash
# Git/SSH setup shared across all sandbox containers.
#
# Sourced (not executed). Caller is responsible for `set -euo pipefail`
# semantics. Reads:
#   WRIX_DEPLOY_KEY  — path to a passphrase-less ed25519 deploy key
#   WRIX_SIGNING_KEY — path to an ed25519 key used for commit signing
#   WRIX_GIT_SIGN    — set to "0" to disable auto-signing (default: on
#                        when WRIX_SIGNING_KEY is set)
#
# Emits a `GIT_SSH_COMMAND` env var and stores the same command in
# git's global `core.sshCommand` so `git push` and other git-over-ssh
# operations use the deploy key, and configures global git signing when
# a signing key is provided. Safe to
# source multiple times and safe to source when no keys are present — it
# becomes a no-op.

write_wrix_ssh_config() {
  local ssh_home="$1"
  [[ -n "$ssh_home" ]] || return 0
  mkdir -p "$ssh_home/.ssh"
  chmod 700 "$ssh_home/.ssh"
  cat > "$ssh_home/.ssh/config" <<SSHEOF
Host github.com
  IdentityFile $WRIX_DEPLOY_KEY
  IdentitiesOnly yes
SSHEOF
  chmod 600 "$ssh_home/.ssh/config"
}

wrix_effective_user_home() {
  local passwd_entry passwd_home
  if passwd_entry=$(getent passwd "$(id -u)"); then
    passwd_home="${passwd_entry#*:*:*:*:*:}"
    printf '%s\n' "${passwd_home%%:*}"
  fi
}

if [[ -n "${WRIX_DEPLOY_KEY:-}" ]] && [[ -f "$WRIX_DEPLOY_KEY" ]]; then
  printf -v WRIX_DEPLOY_KEY_SSH_ARG '%q' "$WRIX_DEPLOY_KEY"
  export GIT_SSH_COMMAND="ssh -i $WRIX_DEPLOY_KEY_SSH_ARG -o IdentitiesOnly=yes"
  git config --global --replace-all core.sshCommand "$GIT_SSH_COMMAND"

  WRIX_EFFECTIVE_HOME=$(wrix_effective_user_home)
  write_wrix_ssh_config "$HOME"
  if [[ -n "$WRIX_EFFECTIVE_HOME" && "$WRIX_EFFECTIVE_HOME" != "$HOME" ]]; then
    write_wrix_ssh_config "$WRIX_EFFECTIVE_HOME"
  fi
  unset WRIX_EFFECTIVE_HOME
fi

if [[ -n "${WRIX_SIGNING_KEY:-}" ]] && [[ -f "$WRIX_SIGNING_KEY" ]]; then
  git config --global gpg.format ssh
  git config --global user.signingkey "$WRIX_SIGNING_KEY"

  mkdir -p "$HOME/.config/git"
  PUBKEY_TMP="$HOME/.config/git/signing_key.pub.tmp"
  # best-effort: ssh-keygen prints "no such file" / "invalid format" to stderr
  # when the key is unreadable; the if-guard already handles failure, the
  # stderr would just clutter container startup logs.
  if ssh-keygen -y -f "$WRIX_SIGNING_KEY" > "$PUBKEY_TMP" 2>/dev/null; then
    echo "${GIT_AUTHOR_EMAIL:-sandbox@wrix.dev} $(cat "$PUBKEY_TMP")" > "$HOME/.config/git/allowed_signers"
    rm "$PUBKEY_TMP"
    git config --global gpg.ssh.allowedSignersFile "$HOME/.config/git/allowed_signers"
  fi

  if [[ "${WRIX_GIT_SIGN:-1}" != "0" ]]; then
    git config --global commit.gpgsign true
  fi
fi
