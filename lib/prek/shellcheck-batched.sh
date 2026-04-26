# shellcheck shell=bash
# Batched wrapper: processes files in groups of 25 to avoid OOM when
# the full set is passed in a single invocation.
# treefmt calls: shellcheck-batched --severity=warning file1.sh file2.sh ...

opts=()
files=()
for arg in "$@"; do
  case "$arg" in
    -*) opts+=("$arg") ;;
    *)  files+=("$arg") ;;
  esac
done

rc=0
for (( i=0; i<${#files[@]}; i+=25 )); do
  # shellcheck disable=SC2086
  shellcheck "${opts[@]}" "${files[@]:i:25}" || rc=$?
done
exit $rc
