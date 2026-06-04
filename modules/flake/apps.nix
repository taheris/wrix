_:

{
  perSystem =
    { test, ... }:
    {
      apps = {
        test = test.app;
        test-base-image-hash-stable = test.apps.base-image-hash-stable;
        test-base-image-universal = test.apps.base-image-universal;
        test-claude-runtime-noop = test.apps.claude-runtime-noop;
        test-customisation-layer-bounded = test.apps.customisation-layer-bounded;
        test-downstream-change-leaf-only = test.apps.downstream-change-leaf-only;
        test-image-digest-matches-stored-id = test.apps.image-digest-matches-stored-id;
        test-image-install-digest-skip = test.apps.image-install-digest-skip;
        test-image-nix-db-consistent = test.apps.image-nix-db-consistent;
        test-iteration-cost-bounded = test.apps.iteration-cost-bounded;
        test-pinned-toolchain-stable-tier = test.apps.pinned-toolchain-stable-tier;
        test-profiles-build-package = test.apps.profiles-build-package;
        test-stable-profile-hash-stable = test.apps.stable-profile-hash-stable;
        test-stable-profile-membership = test.apps.stable-profile-membership;
        test-wrapix-spawn-load = test.apps.wrapix-spawn-load;
      };
    };
}
