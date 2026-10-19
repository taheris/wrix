//! Recorded generation assignments are restart intent, never proof of native ownership or readiness.

use serde::{Deserialize, Serialize};
use wrix_sandbox::image::{Digest, ImageRef};

use super::{Generation, Id};

/// All deployment roles remain required even if a workspace does not use them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Gateway,
    Dolt,
    Celler,
    Compiler,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Gateway, Self::Dolt, Self::Celler, Self::Compiler];
}

/// A package-resolved image assigned to a generation, without runtime admission authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    generation: Generation,
    image: ImageRef,
    digest: Digest,
}

impl Target {
    pub const fn new(generation: Generation, image: ImageRef, digest: Digest) -> Self {
        Self {
            generation,
            image,
            digest,
        }
    }

    pub const fn generation(&self) -> Generation {
        self.generation
    }

    pub const fn image(&self) -> &ImageRef {
        &self.image
    }

    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// Gateway selection and each backend assignment survive disruptive host stop independently.
/// Loading this record must not open data, revive grants or select a transport route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Record {
    deployment: Id,
    gateway: Target,
    dolt: Target,
    celler: Target,
    compiler: Target,
}

impl Record {
    pub fn initial(deployment: Id, target: Target) -> Self {
        Self {
            deployment,
            gateway: target.clone(),
            dolt: target.clone(),
            celler: target.clone(),
            compiler: target,
        }
    }

    pub const fn deployment(&self) -> &Id {
        &self.deployment
    }

    pub const fn target(&self, role: Role) -> &Target {
        match role {
            Role::Gateway => &self.gateway,
            Role::Dolt => &self.dolt,
            Role::Celler => &self.celler,
            Role::Compiler => &self.compiler,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    deployment: Id,
    gateway: Target,
    dolt: Target,
    celler: Target,
    compiler: Target,
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer)?;
        let targets = [&wire.gateway, &wire.dolt, &wire.celler, &wire.compiler];
        for (index, target) in targets.iter().enumerate() {
            if targets[..index].iter().any(|previous| {
                previous.generation == target.generation
                    && (previous.image != target.image || previous.digest != target.digest)
            }) {
                return Err(serde::de::Error::custom(
                    "one generation has conflicting image assignments",
                ));
            }
        }
        Ok(Self {
            deployment: wire.deployment,
            gateway: wire.gateway,
            dolt: wire.dolt,
            celler: wire.celler,
            compiler: wire.compiler,
        })
    }
}
