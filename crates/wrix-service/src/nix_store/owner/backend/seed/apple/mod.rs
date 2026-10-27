//! Apple native image reference/index and protected configuration receipt stay distinct.
use super::{
    Architecture, COMMAND_BUDGET, Configuration, Digest, Error, Id, Observation, OperatingSystem,
    Program, System, Tools, success,
};

pub(super) fn inspect(
    tools: &Tools,
    system: System,
    digest: &Digest,
) -> Result<Observation, Error> {
    use wrix_sandbox::image::{CommandStore, Runtime, Store};
    let (container, skopeo) = tools.apple_image_commands()?;
    wrix_sandbox::image::with_runtime_commands(&container, &skopeo, || {
        let mut store = CommandStore;
        let target = store
            .image_for_digest(Runtime::Container, digest)?
            .ok_or(Error::Invalid)?;
        let native = store
            .image_id(Runtime::Container, &target)?
            .ok_or(Error::Invalid)?;
        if store.image_digest(Runtime::Container, &target)?.as_ref() != Some(digest) {
            return Err(Error::Invalid);
        }
        let mut command = tools.command(Program::Container);
        command.args(["image", "inspect", target.as_str()]);
        let bytes = success(tools.run(command, None, Some(COMMAND_BUDGET))?)?;
        observation(&bytes, system, digest, native.as_str())
    })
}

pub(super) fn observation(
    bytes: &[u8],
    system: System,
    digest: &Digest,
    native: &str,
) -> Result<Observation, Error> {
    let images: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    let [image] = images.as_slice() else {
        return Err(Error::Invalid);
    };
    let index = Digest::parse(
        image
            .pointer("/configuration/descriptor/digest")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Invalid)?,
    )
    .map_err(|_| Error::Invalid)?;
    let id = index
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(Error::Invalid)?;
    if image["id"]
        .as_str()
        .map(|id| id.strip_prefix("sha256:").unwrap_or(id))
        != Some(id)
        || native.strip_prefix("sha256:").unwrap_or(native) != id
    {
        return Err(Error::Invalid);
    }
    let architecture = match system {
        System::X86_64Linux => "amd64",
        System::Aarch64Linux => "arm64",
    };
    let variants = image["variants"].as_array().ok_or(Error::Invalid)?;
    let mut selected = variants.iter().filter(|variant| {
        variant["platform"]["os"] == "linux" && variant["platform"]["architecture"] == architecture
    });
    let variant = selected.next().ok_or(Error::Invalid)?;
    if selected.next().is_some()
        || variant["config"]["os"] != "linux"
        || variant["config"]["architecture"] != architecture
    {
        return Err(Error::Invalid);
    }
    Ok(Observation {
        id: Id::try_from(
            digest
                .as_str()
                .strip_prefix("sha256:")
                .ok_or(Error::Invalid)?
                .to_owned(),
        )?,
        architecture: match system {
            System::X86_64Linux => Architecture::Amd64,
            System::Aarch64Linux => Architecture::Arm64,
        },
        os: OperatingSystem::Linux,
        configuration: Configuration {
            labels: serde_json::from_value(variant["config"]["config"]["Labels"].clone())?,
        },
    })
}
