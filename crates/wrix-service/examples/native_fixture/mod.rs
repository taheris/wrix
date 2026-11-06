mod inventory;
mod maintenance;
mod proxy;
mod shared;
mod sql;

use std::{ffi::OsString, process::ExitCode};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// usage: wrix-native-fixtures sql-client SOCKET [ROLE ARGS] | shared-cli ARGS | backend-runtime ENGINE ARGS
    Usage,
    /// {0}
    Inventory(#[from] inventory::Error),
    /// {0}
    Maintenance(#[from] maintenance::Error),
    /// {0}
    Shared(#[from] shared::Error),
    /// {0}
    Sql(#[from] sql::Error),
    /// {0}
    Proxy(#[from] proxy::Error),
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<ExitCode, Error> {
    let args: Vec<_> = args.into_iter().collect();
    let Some((mode, args)) = args.split_first() else {
        return Err(Error::Usage);
    };
    match mode.to_str() {
        Some("shared-runtime") => Ok(shared::run(args)?),
        Some("inventory") => Ok(inventory::run(args)?),
        Some("maintenance") => Ok(maintenance::run(args)?),
        Some("sql-client") => Ok(sql::run(args)?),
        Some("shared-cli") => Ok(proxy::shared_cli(args)?),
        Some("backend-runtime") => Ok(proxy::backend(args)?),
        _ => Err(Error::Usage),
    }
}
