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

if [[ -n "${WRIX_DEPLOY_KEY:-}" ]] && [[ -f "$WRIX_DEPLOY_KEY" ]]; then
  printf -v WRIX_DEPLOY_KEY_SSH_ARG '%q' "$WRIX_DEPLOY_KEY"
  export GIT_SSH_COMMAND="ssh -i $WRIX_DEPLOY_KEY_SSH_ARG -o IdentitiesOnly=yes"
  git config --global --replace-all core.sshCommand "$GIT_SSH_COMMAND"

  # Write SSH config so bare `ssh -T git@github.com` also works (not just
  # git commands that honour GIT_SSH_COMMAND).
  mkdir -p "$HOME/.ssh"
  chmod 700 "$HOME/.ssh"
  cat > "$HOME/.ssh/config" <<SSHEOF
Host github.com
  IdentityFile $WRIX_DEPLOY_KEY
  IdentitiesOnly yes
SSHEOF
  chmod 600 "$HOME/.ssh/config"
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
