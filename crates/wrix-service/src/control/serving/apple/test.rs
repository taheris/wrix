use super::*;
use serde_json::json;
const INTERFACES: &str = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\tinet 127.0.0.1 netmask 0xff000000\nbridge100: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\tinet 192.168.64.1 netmask 0xffffff00 broadcast 192.168.64.255\n\tmember: vmenet0 flags=3<LEARNING,DISCOVER>\n\tmember: vmenet1 flags=3<LEARNING,DISCOVER>\n";
fn network() -> serde_json::Value {
    json!([{"id":"default","configuration":{"name":"default","mode":"nat","plugin":"container-network-vmnet","options":{},"creationDate":"2026-01-01T00:00:00Z"},"status":{"ipv4Gateway":"192.168.64.1","ipv4Subnet":"192.168.64.0/24"}}])
}
fn resolve(value: &serde_json::Value, interfaces: &str, os: &str) -> io::Result<Address> {
    address(
        &serde_json::to_vec(value).unwrap(),
        interfaces.as_bytes(),
        os.as_bytes(),
    )
}
#[test]
fn reserved_default_vmnet_requires_positive_isolated_host_interface_evidence() {
    let result = resolve(&network(), INTERFACES, "25.0.0\n").unwrap();
    assert_eq!(result.gateway(), Ipv4Addr::new(192, 168, 64, 1));
    assert!(result.accepts(Ipv4Addr::new(192, 168, 64, 17)));
    for denied in [
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(192, 168, 64, 0),
        Ipv4Addr::new(192, 168, 64, 255),
        Ipv4Addr::new(192, 168, 65, 1),
    ] {
        assert!(!result.accepts(denied));
    }
}
#[test]
fn allocation_only_and_non_native_networks_cannot_supply_stable_listener() {
    assert!(resolve(&network(), INTERFACES, "24.6.0").is_err());
    for (pointer, value) in [
        ("/0/configuration/options/variant", json!("allocationOnly")),
        ("/0/configuration/mode", json!("hostOnly")),
        ("/0/configuration/plugin", json!("other")),
        ("/0/id", json!("foreign")),
        ("/0/status/ipv4Subnet", json!("192.168.64.0/33")),
        ("/0/status/ipv4Subnet", json!("192.168.64.1/24")),
    ] {
        let mut native = network();
        // variant is optional in the actual default API resource.
        if pointer.ends_with("/variant") {
            native[0]["configuration"]["options"]["variant"] = value;
        } else {
            *native.pointer_mut(pointer).unwrap() = value;
        }
        assert!(resolve(&native, INTERFACES, "25.0.0").is_err(), "{pointer}");
    }
}
#[test]
fn physical_members_overlap_missing_and_inactive_interfaces_are_denied() {
    for interfaces in [
        INTERFACES.replace("vmenet1", "en0"),
        INTERFACES.replace("bridge100:", "en0:"),
        INTERFACES.replace(",RUNNING,", ","),
        INTERFACES.replace("192.168.64.1 netmask", "192.168.65.1 netmask"),
        format!(
            "{INTERFACES}en0: flags=8863<UP,BROADCAST,RUNNING> mtu 1500\n\tinet 192.168.64.77 netmask 0xffffff00\n"
        ),
    ] {
        assert!(resolve(&network(), &interfaces, "25.0.0").is_err());
    }
}
#[test]
fn ambiguous_network_and_duplicate_gateway_interfaces_fail_closed() {
    let mut duplicate = network();
    duplicate.as_array_mut().unwrap().push(network()[0].clone());
    assert!(resolve(&duplicate, INTERFACES, "25.0.0").is_err());
    assert!(
        resolve(
            &network(),
            &format!(
                "{INTERFACES}{}",
                INTERFACES.replace("bridge100", "bridge101")
            ),
            "25.0.0"
        )
        .is_err()
    );
}
