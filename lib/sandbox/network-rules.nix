rules:
let
  inherit (builtins)
    all
    attrNames
    isAttrs
    isInt
    isList
    isString
    ;
  validRule =
    rule:
    isAttrs rule
    &&
      attrNames rule == [
        "host"
        "ports"
      ]
    && isString rule.host
    && rule.host != ""
    && isList rule.ports
    && rule.ports != [ ]
    && all (port: isInt port && port >= 1 && port <= 65535) rule.ports;
in
if isList rules && all validRule rules then
  rules
else
  throw "networkAllowlist requires { host, ports } rules with nonempty hosts and explicit TCP ports 1-65535"
