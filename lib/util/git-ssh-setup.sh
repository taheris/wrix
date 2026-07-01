# shellcheck shell=bash
# Git/SSH setup shared across all sandbox containers.
#
# Sourced (not executed). Caller is responsible for `set -euo pipefail`
# semantics. Reads:
#   WRIX_DEPLOY_KEY  — path to a passphrase-less ed25519 deploy key
#   WRIX_SIGNING_KEY — path to an ed25519 key used for commit signing
#   WRIX_GIT_SIGN    — set to "0" to disable auto-signing (default: on
#                        when WRIX_SIGNING_KEY is set)
#   GIT_AUTHOR_* / GIT_COMMITTER_* — preferred commit identity values
#
# Emits a pinned `GIT_SSH_COMMAND`, stores the same command in git's global
# `core.sshCommand`, configures global commit identity, and configures SSH
# signing when a signing key is provided. Safe to source multiple times and
# safe to source when no keys are present.

wrix_git_global_value() {
  local key="$1"
  local value
  if value=$(git config --global --get "$key"); then
    [[ -n "$value" ]] || return 1
    printf '%s\n' "$value"
    return 0
  fi
  return 1
}

wrix_first_non_empty() {
  local value
  for value in "$@"; do
    if [[ -n "$value" ]]; then
      printf '%s\n' "$value"
      return 0
    fi
  done
  return 1
}

wrix_configure_git_identity() {
  local author_name author_email committer_name committer_email

  if ! author_name=$(wrix_first_non_empty "${GIT_AUTHOR_NAME:-}" "${GIT_COMMITTER_NAME:-}"); then
    if ! author_name=$(wrix_git_global_value user.name); then
      author_name="Wrix Sandbox"
    fi
  fi
  if ! author_email=$(wrix_first_non_empty "${GIT_AUTHOR_EMAIL:-}" "${GIT_COMMITTER_EMAIL:-}"); then
    if ! author_email=$(wrix_git_global_value user.email); then
      author_email="sandbox@wrix.dev"
    fi
  fi
  if ! committer_name=$(wrix_first_non_empty "${GIT_COMMITTER_NAME:-}"); then
    committer_name="$author_name"
  fi
  if ! committer_email=$(wrix_first_non_empty "${GIT_COMMITTER_EMAIL:-}"); then
    committer_email="$author_email"
  fi

  git config --global --replace-all user.name "$author_name"
  git config --global --replace-all user.email "$author_email"

  export GIT_AUTHOR_NAME="$author_name"
  export GIT_AUTHOR_EMAIL="$author_email"
  export GIT_COMMITTER_NAME="$committer_name"
  export GIT_COMMITTER_EMAIL="$committer_email"
}

write_wrix_ssh_config() {
  local ssh_home="$1"
  [[ -n "$ssh_home" ]] || return 0
  mkdir -p "$ssh_home/.ssh"
  chmod 700 "$ssh_home/.ssh"
  cat > "$ssh_home/.ssh/config" <<SSHEOF
Host github.com
  IdentityFile $WRIX_DEPLOY_KEY
  IdentitiesOnly yes
  StrictHostKeyChecking yes
  UserKnownHostsFile /etc/ssh/ssh_known_hosts
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

wrix_signing_principals() {
  local configured_email="$1"
  local principal seen
  seen=""
  for principal in "$configured_email" "${GIT_AUTHOR_EMAIL:-}" "${GIT_COMMITTER_EMAIL:-}"; do
    [[ -n "$principal" ]] || continue
    case ",$seen," in
      *",$principal,"*) ;;
      *)
        printf '%s\n' "$principal"
        seen="$seen,$principal"
        ;;
    esac
  done
}

wrix_configure_git_signing() {
  local allowed_signers configured_email principal pubkey pubkey_tmp

  git config --global --replace-all gpg.format ssh
  git config --global --replace-all user.signingkey "$WRIX_SIGNING_KEY"

  mkdir -p "$HOME/.config/git"
  pubkey_tmp="$HOME/.config/git/signing_key.pub.tmp"
  allowed_signers="$HOME/.config/git/allowed_signers"
  # best-effort: invalid key material will fail again at commit signing time.
  if ssh-keygen -y -f "$WRIX_SIGNING_KEY" > "$pubkey_tmp" 2>/dev/null; then
    pubkey=$(<"$pubkey_tmp")
    if ! configured_email=$(wrix_git_global_value user.email); then
      configured_email="${GIT_AUTHOR_EMAIL:-sandbox@wrix.dev}"
    fi
    : > "$allowed_signers"
    while IFS= read -r principal; do
      printf '%s %s\n' "$principal" "$pubkey" >> "$allowed_signers"
    done < <(wrix_signing_principals "$configured_email")
    rm -f "$pubkey_tmp"
    git config --global --replace-all gpg.ssh.allowedSignersFile "$allowed_signers"
  else
    rm -f "$pubkey_tmp"
  fi

  if [[ "${WRIX_GIT_SIGN:-1}" = "0" ]]; then
    git config --global --replace-all commit.gpgsign false
  else
    git config --global --replace-all commit.gpgsign true
  fi
}

wrix_configure_git_identity

if [[ -n "${WRIX_DEPLOY_KEY:-}" ]] && [[ -f "$WRIX_DEPLOY_KEY" ]]; then
  printf -v WRIX_DEPLOY_KEY_SSH_ARG '%q' "$WRIX_DEPLOY_KEY"
  export GIT_SSH_COMMAND="ssh -i $WRIX_DEPLOY_KEY_SSH_ARG -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/etc/ssh/ssh_known_hosts"
  git config --global --replace-all core.sshCommand "$GIT_SSH_COMMAND"

  WRIX_EFFECTIVE_HOME=$(wrix_effective_user_home)
  write_wrix_ssh_config "$HOME"
  if [[ -n "$WRIX_EFFECTIVE_HOME" && "$WRIX_EFFECTIVE_HOME" != "$HOME" ]]; then
    write_wrix_ssh_config "$WRIX_EFFECTIVE_HOME"
  fi
  unset WRIX_EFFECTIVE_HOME
fi

if [[ -n "${WRIX_SIGNING_KEY:-}" ]] && [[ -f "$WRIX_SIGNING_KEY" ]]; then
  wrix_configure_git_signing
fi
