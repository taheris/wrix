_:

{
  perSystem =
    { test, ... }:
    {
      apps = {
        test = test.app;
        test-profiles-build-package = test.apps.profiles-build-package;
        test-wrapix-spawn-load = test.apps.wrapix-spawn-load;
        test-pi-runtime-image = test.apps.pi-runtime-image;
        test-claude-runtime-noop = test.apps.claude-runtime-noop;
      };
    };
}
