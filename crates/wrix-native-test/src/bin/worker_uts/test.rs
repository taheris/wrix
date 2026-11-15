use super::*;
use std::fmt::Write as _;
fn status(changed: &str, bits: u64, seccomp: u8) -> String {
    let mut output = format!("Seccomp:\t{seccomp}\n");
    for name in ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"] {
        writeln!(
            output,
            "{name}:\t{:016x}",
            if name == changed { bits } else { 0 }
        )
        .unwrap();
    }
    output
}
#[test]
fn all_five_capability_sets_must_exclude_both_prohibited_privileges() {
    let map = [[0, 100_000, 65_536]];
    boundary(&status("", 0, 2), &map, 0).unwrap();
    for field in ["CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"] {
        for bits in [1 << 21, 1 << 12, (1 << 21) | (1 << 12)] {
            assert!(matches!(
                boundary(&status(field, bits, 2), &map, 0),
                Err(Error::Boundary)
            ));
        }
    }
}
#[test]
fn host_root_unfiltered_processes_and_incomplete_mappings_are_refused() {
    let good = status("", 0, 2);
    for map in [
        vec![],
        vec![[0, 0, 65_536]],
        vec![[1, 100_000, 65_536]],
        vec![[0, 100_000, 1], [1, 0, 1]],
    ] {
        assert!(matches!(boundary(&good, &map, 0), Err(Error::Boundary)));
    }
    assert!(matches!(
        boundary(&status("", 0, 0), &[[0, 100_000, 65_536]], 0),
        Err(Error::Boundary)
    ));
    assert!(matches!(
        boundary(&good, &[[0, 100_000, 65_536]], 1_000),
        Err(Error::Boundary)
    ));
    assert!(field("Seccomp: 2\nSeccomp: 0\n", "Seccomp").is_err());
}
#[test]
fn mapping_rows_are_exact_nonempty_unsigned_triples() {
    assert_eq!(
        mappings("         0     100000      65536\n").unwrap(),
        [[0, 100_000, 65_536]]
    );
    for invalid in ["0 1", "0 1 2 3", "0 -1 2", "0 1 0", "not-a-map"] {
        assert!(mappings(invalid).is_err());
    }
}
