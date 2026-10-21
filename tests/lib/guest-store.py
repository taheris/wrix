import shlex
import subprocess
import tempfile


def register_guest_store(machine, nix_store, roots):
    paths = subprocess.check_output(
        [nix_store, "--query", "--requisites", *roots], text=True
    ).splitlines()
    assert paths
    registration = subprocess.check_output([nix_store, "--dump-db", *paths])
    with tempfile.NamedTemporaryFile(prefix="wrix-guest-registration-") as source:
        source.write(registration)
        source.flush()
        machine.copy_from_host(source.name, "/tmp/wrix-guest-registration")
    machine.succeed(f"{nix_store} --load-db </tmp/wrix-guest-registration")
    machine.succeed("rm /tmp/wrix-guest-registration")
    observed = machine.succeed(
        shlex.join([nix_store, "--query", "--requisites", *roots])
    ).splitlines()
    assert sorted(observed) == sorted(paths)
    machine.succeed(shlex.join([nix_store, "--verify-path", *roots]))
