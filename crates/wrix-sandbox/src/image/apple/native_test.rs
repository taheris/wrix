//! Explicit native export acceptance; receipts and exports stay fixture-owned.
use super::{Digest, ImageRef, Value};
use std::fs;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[ignore = "requires an explicitly selected loaded Apple image and expected config digest"]
fn native_export_binds_exact_index_configuration_and_reference_lookup() -> TestResult {
    let reference = ImageRef::parse(&std::env::var("WRIX_NATIVE_APPLE_IDENTITY_IMAGE")?)?;
    let expected = Digest::parse(&std::env::var("WRIX_NATIVE_APPLE_IDENTITY_CONFIG")?)?;
    let temporary = tempfile::tempdir_in(fs::canonicalize("/tmp")?)?;
    let receipts = tempfile::tempdir_in(fs::canonicalize("/tmp")?)?;
    let before =
        super::super::run_required_output("container", &["image", "inspect", reference.as_str()])?;
    let metadata: Value = serde_json::from_slice(&before.stdout)?;
    let index = super::index(&metadata)?;
    // Native Apple does not accept a bare index ID as an image selector. The
    // production ID adapter discovers a reference and rechecks that observation.
    let identity = super::reported_id(&before.stdout)?;
    let selector = super::reference(&identity)?;
    let selected =
        super::super::run_required_output("container", &["image", "inspect", &selector])?;
    super::require_id(&selected.stdout, &identity)?;
    super::attest_with_directory(&reference, temporary.path(), || {
        Ok(receipts.path().to_owned())
    })?;
    assert_eq!(
        super::read(receipts.path(), &index)?,
        Some(expected.clone())
    );
    let record = receipts
        .path()
        .join(index.as_str().trim_start_matches("sha256:"));
    let bytes = fs::read(&record)?;
    // A warm receipt must avoid exports without rewriting its immutable proof.
    let warm = tempfile::tempdir_in(fs::canonicalize("/tmp")?)?;
    super::attest_with_directory(&reference, warm.path(), || Ok(receipts.path().to_owned()))?;
    assert_eq!(fs::read(&record)?, bytes);
    assert_eq!(fs::read_dir(warm.path())?.count(), 0);
    warm.close()?;
    temporary.close()?;
    receipts.close()?;
    eprintln!(
        "native exported index {} binds configuration {} via {}",
        index.as_str(),
        expected.as_str(),
        selector
    );
    Ok(())
}
