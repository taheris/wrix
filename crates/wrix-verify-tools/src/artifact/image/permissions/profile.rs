//! Factory profile/source/digest agreement accompanying permission inspection.

use super::{Error, Format, bounded, require, run};
use serde_json::Value;
use std::{fs, path::Path};

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, Error> {
    value
        .pointer(name)
        .ok_or(Error::Contract("factory metadata field absent"))
}

fn filename<'a>(value: &'a Value, name: &str) -> Result<&'a Path, Error> {
    Ok(Path::new(
        field(value, name)?
            .as_str()
            .ok_or(Error::Contract("factory path is not text"))?,
    ))
}

fn read(path: &Path) -> Result<Value, Error> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    Ok(serde_json::from_slice(&bounded(&mut file, size)?)?)
}

fn verify(image: &Value) -> Result<(), Error> {
    let config = read(filename(image, "/config")?)?;
    let digest = fs::read_to_string(filename(image, "/digestFile")?)?;
    require(
        field(&config, "/image/digest")? == digest.trim(),
        "factory config digest differs",
    )?;
    require(
        field(&config, "/image/source")? == field(image, "/source")?,
        "factory config source differs",
    )?;
    require(
        field(&config, "/profile/name")? == field(image, "/name")?,
        "factory profile name differs",
    )?;
    require(
        field(&config, "/agent/kind")? == "pi",
        "factory profile agent differs",
    )?;
    if field(&config, "/image/source_kind")? == "nix-descriptor" {
        let descriptor = read(filename(image, "/source")?)?;
        require(
            field(&descriptor, "/digest")? == digest.trim(),
            "factory descriptor digest differs",
        )?;
        require(
            field(&descriptor, "/config/labels/wrix.nix.private")? == "1",
            "factory descriptor private label differs",
        )?;
    }
    Ok(())
}

pub fn inspect(images_file: &Path, archive: &Path) -> Result<(), Error> {
    run(archive, Format::Archive)?;
    let images = read(images_file)?;
    for image in images
        .as_array()
        .ok_or(Error::Contract("factory image list is not an array"))?
    {
        match field(image, "/sourceKind")?.as_str() {
            Some("docker-archive") => run(filename(image, "/source")?, Format::Archive)?,
            Some("nix-descriptor") => run(filename(image, "/layout")?, Format::Oci)?,
            _ => return Err(Error::Contract("unknown factory source kind")),
        }
        verify(image)?;
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_factory_profile_and_descriptor_assertions_reject_corruption() {
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("config.json");
        let descriptor_path = temporary.path().join("descriptor.json");
        let digest_path = temporary.path().join("digest");
        let image = json!({"name":"rust","source":descriptor_path,"config":config_path,"digestFile":digest_path});
        let config = json!({"image":{"source":descriptor_path,"digest":"sha256:fixture","source_kind":"nix-descriptor"},
            "profile":{"name":"rust"},"agent":{"kind":"pi"}});
        let descriptor =
            json!({"digest":"sha256:fixture","config":{"labels":{"wrix.nix.private":"1"}}});
        fs::write(&digest_path, "sha256:fixture\n").unwrap();
        fs::write(&descriptor_path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        verify(&image).unwrap();
        for pointer in [
            "/image/digest",
            "/image/source",
            "/profile/name",
            "/agent/kind",
        ] {
            let mut wrong = config.clone();
            *wrong.pointer_mut(pointer).unwrap() = json!("wrong");
            fs::write(&config_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
            assert!(matches!(verify(&image), Err(Error::Contract(_))));
        }
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        for pointer in ["/digest", "/config/labels/wrix.nix.private"] {
            let mut wrong = descriptor.clone();
            *wrong.pointer_mut(pointer).unwrap() = json!("wrong");
            fs::write(&descriptor_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
            assert!(matches!(verify(&image), Err(Error::Contract(_))));
        }
    }
}
