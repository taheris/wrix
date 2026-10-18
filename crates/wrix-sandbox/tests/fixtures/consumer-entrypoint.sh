#!/usr/bin/env bash
set -euo pipefail

jq -e '.extensions.initial_prompt == "consumer field" and .extensions.repin == true' "${WRIX_SPAWN_CONFIG:?}" >/dev/null
: >"${WRIX_TEST_RUNTIME_STATE:?}/consumer-ran"
