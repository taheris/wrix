# shellcheck shell=bash
# Git/SSH setup shared across all sandbox containers and city agents.
#
# Sourced (not executed). Caller is responsible for `set -euo pipefail`
# semantics. Reads:
#   WRAPIX_DEPLOY_KEY  — path to a passphrase-less ed25519 deploy key
#   WRAPIX_SIGNING_KEY — path to an ed25519 key used for commit signing
#   WRAPIX_GIT_SIGN    — set to "0" to disable auto-signing (default: on
#                        when WRAPIX_SIGNING_KEY is set)
#
# Emits a `GIT_SSH_COMMAND` env var (exported into the caller's shell) so
# `git push` and other git-over-ssh operations use the deploy key, and
# configures global git signing when a signing key is provided. Safe to
# source multiple times and safe to source when no keys are present — it
# becomes a no-op.

if [ -n "${WRAPIX_DEPLOY_KEY:-}" ] && [ -f "$WRAPIX_DEPLOY_KEY" ]; then
  export GIT_SSH_COMMAND="ssh -i $WRAPIX_DEPLOY_KEY -o IdentitiesOnly=yes"

  # Write SSH config so bare `ssh -T git@github.com` also works (not just
  # git commands that honour GIT_SSH_COMMAND).
  mkdir -p "$HOME/.ssh"
  chmod 700 "$HOME/.ssh"
  cat > "$HOME/.ssh/config" <<SSHEOF
Host github.com
  IdentityFile $WRAPIX_DEPLOY_KEY
  IdentitiesOnly yes
SSHEOF
  chmod 600 "$HOME/.ssh/config"
fi

if [ -n "${WRAPIX_SIGNING_KEY:-}" ] && [ -f "$WRAPIX_SIGNING_KEY" ]; then
  git config --global gpg.format ssh
  git config --global user.signingkey "$WRAPIX_SIGNING_KEY"

  mkdir -p "$HOME/.config/git"
  PUBKEY_TMP="$HOME/.config/git/signing_key.pub.tmp"
  if ssh-keygen -y -f "$WRAPIX_SIGNING_KEY" > "$PUBKEY_TMP" 2>/dev/null; then
    echo "${GIT_AUTHOR_EMAIL:-sandbox@wrapix.dev} $(cat "$PUBKEY_TMP")" > "$HOME/.config/git/allowed_signers"
    rm "$PUBKEY_TMP"
    git config --global gpg.ssh.allowedSignersFile "$HOME/.config/git/allowed_signers"
  fi

  if [ "${WRAPIX_GIT_SIGN:-1}" != "0" ]; then
    git config --global commit.gpgsign true
  fi
fi
