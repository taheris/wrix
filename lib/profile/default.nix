{ sandbox }:

{
  deriveProfile =
    baseProfile: extensions:
    baseProfile
    // extensions
    // {
      corePackages = baseProfile.corePackages or [ ];
      packages = (baseProfile.packages or [ ]) ++ (extensions.packages or [ ]);
      hostPackages = (baseProfile.hostPackages or [ ]) ++ (extensions.hostPackages or [ ]);
      mounts = (baseProfile.mounts or [ ]) ++ (extensions.mounts or [ ]);
      env = (baseProfile.env or { }) // (extensions.env or { });
      networkAllowlist = (baseProfile.networkAllowlist or [ ]) ++ (extensions.networkAllowlist or [ ]);
    };

  rustProfile =
    {
      toolchain,
      sha256,
      packages ? [ ],
      hostPackages ? [ ],
      env ? { },
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
      mounts = base.mounts ++ mounts;
      networkAllowlist = base.networkAllowlist ++ networkAllowlist;
    };
}
