//! Server-owned serving and ephemeral egress issuance over shared control authority.

#[path = "gateway/mod.rs"]
pub mod gateway;
#[path = "serving/mod.rs"]
pub mod serving;

use std::{ops::Deref, path::Path, sync::Arc};
use tokio::sync::RwLock;
pub use wrix_service::control::*;

pub struct Controller {
    authority: wrix_service::control::Controller,
    pub(crate) gateway: RwLock<Option<Arc<gateway::Issuer>>>,
}

impl Controller {
    pub fn bind(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            authority: wrix_service::control::Controller::bind(root)?,
            gateway: RwLock::new(None),
        })
    }

    pub fn with_cache_issuer(self, root: &Path) -> Result<Self, Error> {
        Ok(Self {
            authority: self.authority.with_cache_issuer(root)?,
            ..self
        })
    }

    pub fn close(self) -> Result<(), Error> {
        self.authority.close()
    }
}

impl Deref for Controller {
    type Target = wrix_service::control::Controller;

    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}
