{
  mode,
  repoRoot,
  system,
  packageName ? "",
  headless ? "true",
  width ? "1280",
  height ? "720",
  userDataDir ? "/home/wrix/.cache/playwright-mcp",
  configJson ? "{}",
}:

let
  flake = builtins.getFlake ("git+file:" + repoRoot);
  linuxSystem =
    if system == "aarch64-darwin" then
      "aarch64-linux"
    else if system == "x86_64-darwin" then
      "x86_64-linux"
    else
      system;
  pkgs = flake.inputs.nixpkgs.legacyPackages.${linuxSystem};
  wrixLib = flake.legacyPackages.${system}.lib;
  inherit (pkgs.lib) getName recursiveUpdate;

  parseBool =
    value:
    if value == "true" then
      true
    else if value == "false" then
      false
    else
      throw "headless must be true or false";
  parseInt = value: builtins.fromJSON value;

  serverDef = import ../../../lib/mcp/playwright { inherit pkgs; };
  config = recursiveUpdate (builtins.fromJSON configJson) {
    browser = {
      inherit userDataDir;
    };
  };
  mcpOptions = {
    headless = parseBool headless;
    viewport = {
      width = parseInt width;
      height = parseInt height;
    };
    inherit config;
  };
  configJSON = serverDef.passthru.mkConfig mcpOptions;
  serverConfig = serverDef.mkServerConfig mcpOptions;
  configPath = builtins.elemAt serverConfig.args 1;
  configRealizer = pkgs.runCommand "playwright-mcp-config-realizer" { } ''
    set -euo pipefail

    cp ${configPath} "$out"
  '';
  packageMatches = builtins.filter (pkg: getName pkg == packageName) serverDef.packages;
  packageByName =
    if packageMatches == [ ] then
      throw "unknown playwright package '${packageName}'"
    else
      builtins.head packageMatches;
  networkDenyPreload = pkgs.runCommandCC "wrix-playwright-deny-network-preload" { } ''
        set -euo pipefail

        mkdir -p "$out/lib"
        cat > deny-network.c <<'EOF'
    #define _GNU_SOURCE
    #include <dlfcn.h>
    #include <errno.h>
    #include <fcntl.h>
    #include <netinet/in.h>
    #include <stdio.h>
    #include <stdlib.h>
    #include <sys/socket.h>
    #include <unistd.h>

    static int is_inet_address(const struct sockaddr *addr) {
      return addr != NULL && (addr->sa_family == AF_INET || addr->sa_family == AF_INET6);
    }

    static void record_attempt(const char *call_name, const struct sockaddr *addr) {
      const char *log_path = getenv("WRIX_NETWORK_DENY_LOG");
      if (log_path == NULL || log_path[0] == '\0') {
        return;
      }

      int fd = open(log_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
      if (fd < 0) {
        return;
      }

      (void)dprintf(fd, "%s family=%d\n", call_name, addr->sa_family);
      (void)close(fd);
    }

    typedef int (*connect_fn)(int, const struct sockaddr *, socklen_t);
    typedef ssize_t (*sendmsg_fn)(int, const struct msghdr *, int);
    typedef ssize_t (*sendto_fn)(int, const void *, size_t, int, const struct sockaddr *, socklen_t);

    int connect(int sockfd, const struct sockaddr *addr, socklen_t addrlen) {
      if (is_inet_address(addr)) {
        record_attempt("connect", addr);
        errno = ENETUNREACH;
        return -1;
      }

      connect_fn real_connect = (connect_fn)dlsym(RTLD_NEXT, "connect");
      if (real_connect == NULL) {
        errno = ENOSYS;
        return -1;
      }
      return real_connect(sockfd, addr, addrlen);
    }

    ssize_t sendmsg(int sockfd, const struct msghdr *msg, int flags) {
      const struct sockaddr *addr = msg == NULL ? NULL : (const struct sockaddr *)msg->msg_name;
      if (is_inet_address(addr)) {
        record_attempt("sendmsg", addr);
        errno = ENETUNREACH;
        return -1;
      }

      sendmsg_fn real_sendmsg = (sendmsg_fn)dlsym(RTLD_NEXT, "sendmsg");
      if (real_sendmsg == NULL) {
        errno = ENOSYS;
        return -1;
      }
      return real_sendmsg(sockfd, msg, flags);
    }

    ssize_t sendto(int sockfd, const void *buf, size_t len, int flags, const struct sockaddr *dest_addr, socklen_t addrlen) {
      if (is_inet_address(dest_addr)) {
        record_attempt("sendto", dest_addr);
        errno = ENETUNREACH;
        return -1;
      }

      sendto_fn real_sendto = (sendto_fn)dlsym(RTLD_NEXT, "sendto");
      if (real_sendto == NULL) {
        errno = ENOSYS;
        return -1;
      }
      return real_sendto(sockfd, buf, len, flags, dest_addr, addrlen);
    }
    EOF
        cc -shared -fPIC deny-network.c -ldl -o "$out/lib/libwrix-deny-network.so"
  '';
  sandbox = wrixLib.mkSandbox {
    profile = wrixLib.profiles.base;
    mcp = {
      playwright = mcpOptions;
    };
  };
  sandboxPackageClosure = pkgs.closureInfo {
    rootPaths = sandbox.profile.packages;
  };
  outputs = {
    config-json = builtins.toJSON configJSON;
    config-path = configPath;
    config-realizer = configRealizer;
    server-args-json = builtins.toJSON serverConfig.args;
    server-command = serverConfig.command;
    server-env-json = builtins.toJSON serverConfig.env;
    server-registry-json = builtins.toJSON {
      inherit (serverDef) name;
      packageNames = map getName serverDef.packages;
      mkServerConfigIsFunction = builtins.isFunction serverDef.mkServerConfig;
      sampleConfig = {
        inherit (serverConfig) command args env;
      };
    };
    chromium-executable-path = configJSON.browser.launchOptions.executablePath;
    network-deny-preload = networkDenyPreload;
    package = packageByName;
    package-path = toString packageByName;
    sandbox-image = sandbox.image;
    sandbox-image-path = toString sandbox.image;
    sandbox-profile-package-names-json = builtins.toJSON (map getName sandbox.profile.packages);
    sandbox-profile-package-paths-json = builtins.toJSON (map toString sandbox.profile.packages);
    sandbox-package-closure = sandboxPackageClosure;
  };
in
outputs.${mode} or (throw "unknown playwright eval mode '${mode}'")
