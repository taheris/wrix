_:

{
  perSystem =
    { test, ... }:
    {
      apps = {
        test = test.app;
        test-ci = test.apps.ci;
        test-agent-exclusive = test.apps.agent-exclusive;
        test-agent-pkg-threaded = test.apps.agent-pkg-threaded;
        test-agent-tier-isolated = test.apps.agent-tier-isolated;
        test-archiveless-generated-change = test.apps.archiveless-generated-change;
        test-base-image-hash-stable = test.apps.base-image-hash-stable;
        test-base-image-universal = test.apps.base-image-universal;
        test-claude-runtime-noop = test.apps.claude-runtime-noop;
        test-customisation-layer-bounded = test.apps.customisation-layer-bounded;
        test-downstream-change-leaf-only = test.apps.downstream-change-leaf-only;
        test-entrypoint-resolver-base = test.apps.entrypoint-resolver-base;
        test-image-digest-matches-stored-id = test.apps.image-digest-matches-stored-id;
        test-image-digest-no-tar = test.apps.image-digest-no-tar;
        test-image-install-real-skopeo = test.apps.image-install-real-skopeo;
        test-image-install-archiveless = test.apps.image-install-archiveless;
        test-image-install-digest-skip = test.apps.image-install-digest-skip;
        test-image-nix-db-consistent = test.apps.image-nix-db-consistent;
        test-image-nix-db-no-dangling = test.apps.image-nix-db-no-dangling;
        test-image-tier-graph = test.apps.image-tier-graph;
        test-image-tier-membership = test.apps.image-tier-membership;
        test-iteration-cost-bounded = test.apps.iteration-cost-bounded;
        test-linux-image-archiveless-source = test.apps.linux-image-archiveless-source;
        test-notify = test.apps.notify;
        test-pinned-toolchain-stable-tier = test.apps.pinned-toolchain-stable-tier;
        test-prek-hooks-closure = test.apps.prek-hooks-closure;
        test-profiles-build-package = test.apps.profiles-build-package;
        test-stable-profile-hash-stable = test.apps.stable-profile-hash-stable;
        test-stable-profile-membership = test.apps.stable-profile-membership;
        test-wrix-image-labels = test.apps.wrix-image-labels;
        test-wrix-images-source-kind = test.apps.wrix-images-source-kind;
        test-wrix-spawn-load = test.apps.wrix-spawn-load;
      };
    };
}
