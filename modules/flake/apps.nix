_:

{
  perSystem =
    { test, ... }:
    {
      apps = {
        test = test.app;
        test-profiles-build-package = test.apps.profiles-build-package;
        test-wrapix-spawn-load = test.apps.wrapix-spawn-load;
        test-claude-runtime-noop = test.apps.claude-runtime-noop;
        test-image-install-digest-skip = test.apps.image-install-digest-skip;
        test-base-image-universal = test.apps.base-image-universal;
      };
    };
}
