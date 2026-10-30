#!/usr/bin/env python3
"""Read-only Podman boundary fixture; reject every mutating command."""
import json
import pathlib
import sys

root = pathlib.Path(__file__).resolve().parent
containers = json.loads((root / "runtime.json").read_text())
args = sys.argv[1:]
with (root / "calls").open("a") as log:
    log.write(json.dumps(args) + "\n")
if args == ["ps", "-a", "--format", "{{.Names}}"]:
    print("\n".join(containers))
elif args[0] == "port" and len(args) == 2:
    sys.exit(0)
elif args[:2] == ["inspect", "--format"] and args[-1] == "--type=container":
    template, name = args[2:4]
    if name not in containers:
        sys.stderr.write("Error: no such container\n")
        sys.exit(1)
    item = containers[name]
    if template == "{{.State.Running}}":
        print(str(item["running"]).lower())
    elif template == "{{.Id}}":
        print(item["id"])
    elif template == "{{json .Mounts}}":
        print(json.dumps(item["mounts"]))
    elif template.startswith('{{ index .Config.Labels "'):
        label = template.split('"')[1]
        print(item["labels"].get(label, "<no value>"))
    else:
        sys.stderr.write("unsupported inspection: " + template)
        sys.exit(1)
else:
    sys.stderr.write("unsupported/mutating runtime command")
    sys.exit(1)
