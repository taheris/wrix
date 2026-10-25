//! Recorded service-VM limits; resume preserves the original profile rather than enlarging it implicitly.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Default, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Budget {
    #[default]
    Legacy,
    HostIo,
}
impl Budget {
    pub(super) const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }
    pub(super) const fn arguments(&self) -> &'static [&'static str] {
        match self {
            Self::Legacy => &["--memory=4g"],
            Self::HostIo => &[
                "--memory=6g",
                "--annotation=krun.ram_mib=4096",
                "--annotation=krun.cpus=2",
            ],
        }
    }
    pub(super) fn matches(&self, memory: u64, annotations: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Legacy => {
                memory == 4_294_967_296
                    && !annotations.contains_key("krun.ram_mib")
                    && !annotations.contains_key("krun.cpus")
            }
            Self::HostIo => {
                memory == 6_442_450_944
                    && annotations.get("krun.ram_mib").map(String::as_str) == Some("4096")
                    && annotations.get("krun.cpus").map(String::as_str) == Some("2")
            }
        }
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn explicit_host_io_headroom_does_not_enlarge_guest_memory_or_native_work() {
        let annotations = BTreeMap::from([
            ("krun.ram_mib".to_owned(), "4096".to_owned()),
            ("krun.cpus".to_owned(), "2".to_owned()),
        ]);
        assert!(Budget::HostIo.matches(6_442_450_944, &annotations));
        assert!(!Budget::HostIo.matches(4_294_967_296, &annotations));
        assert!(!Budget::Legacy.matches(6_442_450_944, &annotations));
        assert!(Budget::Legacy.matches(4_294_967_296, &BTreeMap::new()));
        assert_eq!(
            Budget::HostIo.arguments(),
            [
                "--memory=6g",
                "--annotation=krun.ram_mib=4096",
                "--annotation=krun.cpus=2"
            ]
        );
    }
}
