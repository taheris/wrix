{ sandbox }:

{
  deriveProfile =
    baseProfile@{
      corePackages ? [ ],
      packages ? [ ],
      hostPackages ? [ ],
      mounts ? [ ],
      env ? { },
      hostEnv ? env,
      runtimeSecrets ? { },
      networkAllowlist ? [ ],
      ...
    }:
    let
      baseCorePackages = corePackages;
      basePackages = packages;
      baseHostPackages = hostPackages;
      baseMounts = mounts;
      baseEnv = env;
      baseHostEnv = hostEnv;
      baseRuntimeSecrets = runtimeSecrets;
      baseNetworkAllowlist = networkAllowlist;
    in
    extensions@{
      packages ? [ ],
      hostPackages ? [ ],
      mounts ? [ ],
      env ? { },
      hostEnv ? { },
      runtimeSecrets ? { },
      networkAllowlist ? [ ],
      ...
    }:
    baseProfile
    // extensions
    // {
      corePackages = baseCorePackages;
      packages = basePackages ++ packages;
      hostPackages = baseHostPackages ++ hostPackages;
      mounts = baseMounts ++ mounts;
      env = baseEnv // env;
      hostEnv = baseHostEnv // env // hostEnv;
      runtimeSecrets = sandbox.validateRuntimeSecrets (baseRuntimeSecrets // runtimeSecrets);
      networkAllowlist = sandbox.validateNetworkRules (baseNetworkAllowlist ++ networkAllowlist);
    };

  rustProfile =
    {
      toolchain,
      sha256,
      packages ? [ ],
      hostPackages ? [ ],
      env ? { },
      hostEnv ? { },
      runtimeSecrets ? { },
      mounts ? [ ],
      networkAllowlist ? [ ],
    }:
    let
      base = sandbox.rustProfileFromFile {
        file = toolchain;
        inherit sha256;
      };
    in
    base
    // {
      packages = base.packages ++ packages;
      hostPackages = (base.hostPackages or [ ]) ++ hostPackages;
      env = base.env // env;
      hostEnv = (base.hostEnv or { }) // env // hostEnv;
      runtimeSecrets = sandbox.validateRuntimeSecrets ((base.runtimeSecrets or { }) // runtimeSecrets);
      mounts = base.mounts ++ mounts;
      networkAllowlist = sandbox.validateNetworkRules (base.networkAllowlist ++ networkAllowlist);
    };
}
