_:

{
  perSystem =
    { test, ... }:
    {
      apps = {
        test = test.app;
        test-ci = test.apps.ci;
      };
    };
}
