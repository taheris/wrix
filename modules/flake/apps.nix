_:

{
  perSystem =
    {
      wrapix,
      city,
      test,
      ...
    }:
    {
      apps = {
        city = city.app;
        init = wrapix.ralphInitApp;
        ralph = city.ralph.app;
        test = test.app;
        test-city = test.apps.city;
        test-ralph = test.apps.ralph;
        test-ralph-container = test.apps.ralph-container;
      };
    };
}
