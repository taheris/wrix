{ linuxPkgs }:

let
  runtime = import ../../lib/sandbox/linux/krun-runtime.nix { inherit linuxPkgs; };
in
assert runtime.doCheck && runtime.doInstallCheck;
runtime.overrideAttrs (old: {
  pname = "crun-ring-buffer-constrained-check";
  postCheck = (old.postCheck or "") + ''
    set -euo pipefail
    "$CC" -Wall -Wextra -Werror -DWRIX_PIPE_FIXTURE_SELF_TEST \
      "${./crun-constrained-pipe.c}" -o constrained-pipe-self-test
    ./constrained-pipe-self-test
    "$CC" -Wall -Wextra -Werror -c "${./crun-constrained-pipe.c}" -o constrained-pipe.o

    run_constrained_suite() {
      local output="$1"
      touch tests/tests_libcrun_ring_buffer.c
      make tests/tests_libcrun_ring_buffer \
        tests_tests_libcrun_ring_buffer_CFLAGS='$(WARN_CFLAGS) -I ./libocispec/src $(JSON_C_CFLAGS) -I ./src -Dpipe2=wrix_test_pipe2 -Dfcntl=wrix_test_fcntl' \
        tests_tests_libcrun_ring_buffer_LDADD='$(TESTS_LDADD) constrained-pipe.o'
      ./tests/tests_libcrun_ring_buffer > "$output"
    }

    cp tests/tests_libcrun_ring_buffer.c patched-ring-buffer.c
    cp "${linuxPkgs.crun.src}/tests/tests_libcrun_ring_buffer.c" tests/tests_libcrun_ring_buffer.c
    run_constrained_suite upstream.tap
    diff -u <(printf '%s\n' \
      '1..6' \
      'not ok 1 - test_ring_buffer_read_write' \
      'ok 2 - test_ring_buffer_wraparound_data_integrity' \
      'ok 3 - test_ring_buffer_reserved_byte_boundary' \
      'ok 4 - test_ring_buffer_no_reserved_byte_access' \
      'not ok 5 - test_ring_buffer_wraparound_partial_drain' \
      'not ok 6 - test_ring_buffer_stress_partial_drain') upstream.tap

    cp patched-ring-buffer.c tests/tests_libcrun_ring_buffer.c
    run_constrained_suite patched.tap
    diff -u <(printf '%s\n' \
      '1..6' \
      'ok 1 - test_ring_buffer_read_write' \
      'ok 2 - test_ring_buffer_wraparound_data_integrity' \
      'ok 3 - test_ring_buffer_reserved_byte_boundary' \
      'ok 4 - test_ring_buffer_no_reserved_byte_access' \
      'ok 5 - test_ring_buffer_wraparound_partial_drain' \
      'ok 6 - test_ring_buffer_stress_partial_drain') patched.tap
    mkdir -p "$out/share/crun-pipe-check"
    cp upstream.tap patched.tap "$out/share/crun-pipe-check/"
  '';
})
