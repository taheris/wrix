//! Explicit host approval and membership; identifiers alone never admit a launch.

pub mod admission;
mod record;

use crate::{
    context::{Context, Selection},
    host::Host,
    store::Store,
};
use record::{Catalog, Common, OWNER, Record};
use wrix_core::nix_store::pool::Repository;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// operating-system repository identity generation failed: {0}
    Random(#[from] getrandom::Error),
    /// new repository identity collided; no approval was written; retry
    IdentityCollision,
    /// approval may be committed, but selected workspace confirmation failed: {0}
    Confirmation(#[source] crate::store::Error),
    /// {0}
    State(#[from] crate::store::Error),
    /// {0}
    Host(#[from] crate::host::Error),
    /// {0}
    Resolver(#[from] crate::resolver::Error),
    /// installed-store approval I/O failed: {0}
    Io(#[from] std::io::Error),
    /// installed-store approval database failed: {0}
    Sql(#[from] rusqlite::Error),
    /// malformed installed-store approval metadata: {0}
    Json(#[from] serde_json::Error),
    /// unsupported installed-store approval metadata; explicit host reconciliation is required
    Unsupported,
    /// inconsistent canonical repository approval; explicit host reconciliation is required
    Inconsistent,
    /// installed-store admission requires current explicit checked repository approval
    ApprovalRequired,
    /// installed-store approval requires an actual Git repository
    RepositoryRequired,
    /// approved repository administration changed; explicit host reconciliation is required
    RepositoryChanged,
}

#[derive(Clone, Debug)]
pub enum Approval {
    Unavailable,
    Unapproved,
    Bound {
        repository: Repository,
        enabled: bool,
    },
}

/// Passive metadata, not proof of a ready owner, build isolation or export.
pub fn inspect(selection: &Selection) -> Result<Approval, Error> {
    let Some(context) = selection.context() else {
        return Ok(Approval::Unavailable);
    };
    let host = Host::discover()?;
    context.recheck(&host)?;
    let common = Common::observe(context)?;
    let Some(store) = Store::inspect(&host)? else {
        return Ok(Approval::Unapproved);
    };
    let catalog = Catalog::load(&store)?;
    let approval = catalog
        .selected(context, &common)?
        .map_or(Approval::Unapproved, |record| Approval::Bound {
            repository: record.repository.clone(),
            enabled: record.enabled,
        });
    context.recheck(&host)?;
    Ok(approval)
}

/// The caller must separately check its recorded deployment before enabling.
/// Only an explicit host operation may bind a new namespace to a repository.
pub fn set(host: &Host, context: &Context, enabled: bool) -> Result<(), Error> {
    host.check()?;
    context.recheck(host)?;
    let common = Common::observe(context)?;
    // Reject unknown records before allocating even ordinary namespace metadata.
    if let Some(store) = Store::inspect(host)? {
        Catalog::load(&store)?.selected(context, &common)?;
    }
    context.authorize_effects(host)?;
    let mut store = Store::mutate(host, false)?;
    context.check_store(host, Some(&store))?;
    let mut catalog = Catalog::load(&store)?;
    let selected = catalog.selected(context, &common)?;
    if selected.is_some_and(|record| record.enabled == enabled) {
        drop(store);
        context.recheck(host)?;
        return Ok(());
    }
    if Common::observe(context)? != common {
        return Err(Error::RepositoryChanged);
    }
    let repository =
        if let Some((_, record)) = catalog.0.iter().find(|(_, record)| record.common == common) {
            record.repository.clone()
        } else {
            let repository = new_repository()?;
            if catalog
                .0
                .iter()
                .any(|(_, record)| record.repository == repository)
            {
                return Err(Error::IdentityCollision);
            }
            repository
        };
    if !catalog.0.iter().any(|(key, _)| key == context.key()) {
        catalog.0.push((
            context.key().clone(),
            Record::new(repository.clone(), common, enabled),
        ));
    }
    let tx = store
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for (key, record) in &mut catalog.0 {
        if record.repository == repository {
            record.enabled = enabled;
            tx.execute("INSERT INTO namespace_metadata VALUES (?1, ?2, ?3) ON CONFLICT(native_key, owner) DO UPDATE SET value = excluded.value",
                rusqlite::params![key.as_str(), OWNER, serde_json::to_vec(record)?])?;
        }
    }
    tx.commit()?;
    drop(store);
    context.recheck(host).map_err(Error::Confirmation)?;
    Ok(())
}

fn new_repository() -> Result<Repository, Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    let value = format!("{:032x}", u128::from_be_bytes(bytes));
    Repository::try_from(value).map_err(|_| Error::Unsupported)
}
