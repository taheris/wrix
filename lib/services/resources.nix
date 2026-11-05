{ lib }:
resources:
if resources == null then
  null
else
  assert lib.assertMsg (
    builtins.isAttrs resources
    &&
      builtins.attrNames resources == [
        "cpus"
        "memoryMiB"
      ]
  ) "serviceResources must contain only cpus and memoryMiB";
  assert lib.assertMsg (
    builtins.isInt resources.cpus && resources.cpus >= 1 && resources.cpus <= 256
  ) "serviceResources.cpus must be an integer between 1 and 256";
  assert lib.assertMsg (
    builtins.isInt resources.memoryMiB && resources.memoryMiB >= 512 && resources.memoryMiB <= 1048576
  ) "serviceResources.memoryMiB must be an integer between 512 and 1048576";
  {
    inherit (resources) cpus;
    memory_mib = resources.memoryMiB;
  }
