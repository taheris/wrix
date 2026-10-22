use wrix_core::{
    nix_store::{Mode, Preference},
    repository_policy,
};

#[test]
fn installed_store_selection_is_strict_and_independent() {
    for (value, expected) in [("private", Mode::Private), ("shared", Mode::Shared)] {
        assert_eq!(Mode::parse(value).unwrap(), expected);
        let policy = repository_policy::parse(&format!("[nix_store]\nmode = '{value}'\n")).unwrap();
        assert_eq!(policy.nix_store.mode, Some(expected));
        assert_eq!(policy.tool_caches.shared, None);
        for invocation in [None, Some(Mode::Private), Some(Mode::Shared)] {
            assert_eq!(
                Preference { mode: invocation }.resolve(policy.nix_store),
                invocation.unwrap_or(expected)
            );
        }
    }
    assert_eq!(
        Preference::default().resolve(Preference::default()),
        Mode::Private
    );
    for value in ["", "SHARED", "Private", "shared ", "auto", "disabled"] {
        assert!(Mode::parse(value).is_err());
    }
}

#[test]
fn owned_store_policy_rejects_values_and_authority_fields() {
    for invalid in [
        "nix_store = 'shared'",
        "nix_store = false",
        "[nix_store]\nmode = true",
        "[nix_store]\nmode = 1",
        "[nix_store]\nmode = 'auto'",
        "[nix_store]\napproved = true",
        "[nix_store]\npool = 'peer'",
        "[nix_store]\nendpoint = 'unix:///peer'",
        "[nix_store]\nmode = 'private'\nmode = 'shared'",
    ] {
        assert!(repository_policy::parse(invalid).is_err(), "{invalid}");
    }
    for invalid in [
        "null",
        "false",
        "\"shared\"",
        "{\"mode\":null}",
        "{\"mode\":true}",
        "{\"mode\":\"SHARED\"}",
        "{\"approved\":true}",
        "{\"mode\":\"shared\",\"mode\":\"private\"}",
    ] {
        assert!(
            serde_json::from_str::<Preference>(invalid).is_err(),
            "{invalid}"
        );
    }
}
