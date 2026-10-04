#!/usr/bin/env bash
set -euo pipefail

TEST_TMP=$(mktemp -d -t wrix-git-helper-parity.XXXXXX)
trap 'rm -rf "$TEST_TMP"' EXIT
export HOME="$TEST_TMP/home"
export GIT_CONFIG_NOSYSTEM=1
unset GIT_CONFIG_GLOBAL WRIX_DEPLOY_KEY WRIX_SIGNING_KEY
while IFS= read -r name; do
  unset "$name"
done < <(git rev-parse --local-env-vars)
mkdir -p "$HOME/.ssh/deploy_keys" "$TEST_TMP/repo"
ssh-keygen -t ed25519 -N "" -q -f "$HOME/.ssh/deploy_keys/parity"
ssh-keygen -t ed25519 -N "" -q -f "$HOME/.ssh/deploy_keys/parity-signing"
cd "$TEST_TMP/repo"
git init -q -b main
git remote add origin git@github.com:example/parity.git
git config user.name Fixture
git config user.email fixture@example.invalid
wrix-git-sign --wrix-probe
wrix init --offline --key parity
[[ "$(git config gpg.ssh.program)" == wrix-git-sign ]]
git commit --allow-empty -qm "Sign with the image helper"
git verify-commit HEAD
printf 'PASS: Linux fixture image tools initialize, sign, and verify a repository\n'
