use super::fixture::source as fixture;
use super::*;
#[test]
fn accepts_http_transport_with_typed_public_key_and_split_image() {
    check(&fixture()).unwrap();
}
#[test]
fn missing_each_required_transport_token_is_rejected_with_the_original_diagnostic() {
    for (index, region) in REGIONS.into_iter().enumerate() {
        for needle in region.required() {
            let mut sources = fixture();
            sources[index] = sources[index].replace(needle, "");
            assert_eq!(
                check(&sources).unwrap_err().to_string(),
                format!("FAIL: {region}: missing {}", Quoted::new(needle))
            );
        }
    }
}
#[test]
fn forbidden_transport_in_each_original_checked_location_is_rejected() {
    let locations = [
        (0, Region::Launcher, "    fn load_project_cache() {"),
        (1, Region::Owner, "pub(in crate::lifecycle) fn ensure() {"),
        (2, Region::Server, "    let mut celler ="),
        (3, Region::Image, "  stableContents = ["),
        (3, Region::Image, "  contents = stableContents"),
        (3, Region::Image, "  rawImage = {"),
    ];
    for (index, region, marker) in locations {
        for needle in FORBIDDEN {
            let mut sources = fixture();
            sources[index] = sources[index].replace(marker, &format!("{marker}\n\"{needle}\""));
            assert_eq!(
                check(&sources).unwrap_err().to_string(),
                format!("FAIL: {region}: unexpected {}", Quoted::new(needle))
            );
        }
    }
}
#[test]
fn missing_each_region_marker_fails_closed_before_token_checks() {
    for (index, region) in REGIONS.into_iter().enumerate() {
        let (start, end) = region.markers();
        let mut sources = fixture();
        sources[index] = sources[index].replace(start, "");
        assert_eq!(
            check(&sources).unwrap_err().to_string(),
            format!(
                "FAIL: {region}: missing start marker {}",
                Quoted::new(start)
            )
        );
        let mut sources = fixture();
        sources[index] = sources[index].replace(end, "");
        assert_eq!(
            check(&sources).unwrap_err().to_string(),
            format!("FAIL: {region}: missing end marker {}", Quoted::new(end))
        );
    }
}
#[test]
fn literal_diagnostics_keep_quote_selection_and_escaped_control_characters() {
    assert_eq!(Quoted::new("a\n\\b\t").to_string(), "'a\\n\\\\b\\t'");
    assert_eq!(Quoted::new("a'b").to_string(), "\"a'b\"");
    assert_eq!(Quoted::new("a'\"b").to_string(), "'a\\'\"b'");
}
#[test]
fn forbidden_tokens_outside_the_original_regions_do_not_change_a_verdict() {
    let mut sources = fixture();
    for source in &mut sources {
        source.push_str("\n// file:// unix:// nix-daemon harmonia nix-serve\n");
    }
    check(&sources).unwrap();
}
