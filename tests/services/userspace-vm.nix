{ pkgs }:

pkgs.testers.runNixOSTest {
  name = "services-userspace-vm-smoke";
  requiredFeatures.kvm = false;
  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    virtualisation.memorySize = 512;
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("test $(findmnt -n -o FSTYPE /nix/.ro-store) = 9p")
    machine.succeed("test $(findmnt -n -o FSTYPE /tmp/shared) = 9p")
    machine.fail("touch /nix/.ro-store/wrix-read-only-export-negative-control")
    machine.succeed("printf 'guest-ready\\n' >/tmp/shared/probe; grep -Fx guest-ready /tmp/shared/probe")
  '';
}
