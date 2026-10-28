//! Controller-owned native route attachment and caller-independent obsolete-gateway retirement.

use super::super::{Controller, Error};
use crate::{
    control::gateway::Issuer,
    lifecycle::generation::{Gateway, Plan, State, Status},
    route::{Generation, Role, Selection},
};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

struct Serving {
    plan: Plan,
    routes: Generation,
    issuer: Arc<Issuer>,
}
struct Retiring {
    gateway: Gateway,
    routes: Generation,
}

async fn removed(gateway: &Gateway, authority: &Path) -> Result<bool, Error> {
    let gateway = gateway.clone();
    let root = authority.to_owned();
    tokio::task::spawn_blocking(move || gateway.removed(&root))
        .await
        .map_err(std::io::Error::other)?
        .map_err(|error| std::io::Error::other(error).into())
}

pub(super) async fn run(controller: &Controller, selection: Selection) -> Result<(), Error> {
    let authority = controller.authority_root()?;
    let state = authority
        .parent()
        .and_then(Path::parent)
        .ok_or(Error::PrivateRoot)?
        .to_owned();
    let mut serving: Option<Serving> = None;
    let mut retiring = BTreeMap::<u64, Retiring>::new();
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    loop {
        timer.tick().await;
        let plan = match Plan::load(&authority) {
            Ok(Some(plan)) => plan,
            Ok(None) => continue,
            Err(error) => {
                tracing::error!(%error, "native generation intent is unreadable; existing protected routes retained");
                continue;
            }
        };
        if plan.deployment() != controller.deployment_id() {
            return Err(Error::InvalidSelection);
        }
        let result = reconcile(
            controller,
            &selection,
            &authority,
            &state,
            &plan,
            &mut serving,
            &mut retiring,
        )
        .await;
        if let Err(error) = result {
            tracing::error!(%error, "native generation attachment or retirement remains incomplete");
            publish(&authority, &plan, State::Failed, retiring.len()).await?;
        }
    }
}

async fn reconcile(
    controller: &Controller,
    selection: &Selection,
    authority: &Path,
    state: &Path,
    plan: &Plan,
    serving: &mut Option<Serving>,
    retiring: &mut BTreeMap<u64, Retiring>,
) -> Result<(), Error> {
    let changed = serving
        .as_ref()
        .is_none_or(|old| !old.plan.same_gateway(plan) || !old.plan.same_backend(plan));
    if changed {
        let candidate = plan.clone();
        let state = state.to_path_buf();
        tokio::task::spawn_blocking(move || candidate.ready(&state))
            .await
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        plan.gateway()
            .ready()
            .await
            .map_err(std::io::Error::other)?;
        let gateway = plan.gateway().client();
        let issuer = if let Some(old) = serving.as_ref() {
            old.issuer
                .handoff(gateway)
                .await
                .map_err(std::io::Error::other)?;
            old.issuer.clone()
        } else {
            Arc::new(Issuer::new(gateway))
        };
        let mut routes = Generation::from_native(plan.routes())?;
        if let Some(old) = serving.as_ref() {
            if old.plan.same_gateway(plan) {
                routes.reuse_role(&old.routes, Role::Egress);
            }
            if old.plan.same_backend(plan) {
                for role in [Role::Sql, Role::CacheRead, Role::Compiler, Role::Sync] {
                    routes.reuse_role(&old.routes, role);
                }
            }
        }
        for role in Role::ALL {
            if role != Role::Egress && !plan.backend_open() {
                selection.close(role)?;
            } else {
                selection.select(routes.confirm_ready(role))?;
            }
        }
        if let Some(old) = serving.as_ref()
            && !old.plan.same_gateway(plan)
        {
            old.plan
                .gateway()
                .client()
                .drain(true)
                .await
                .map_err(std::io::Error::other)?;
            retiring.insert(
                old.plan.gateway().generation().number().get(),
                Retiring {
                    gateway: old.plan.gateway().clone(),
                    routes: old.routes.clone(),
                },
            );
        }
        *controller.gateway.write().await = Some(issuer.clone());
        *serving = Some(Serving {
            plan: plan.clone(),
            routes,
            issuer,
        });
    }
    for gateway in plan.obsolete() {
        if retiring.contains_key(&gateway.generation().number().get()) {
            continue;
        }
        if removed(gateway, authority).await? {
            continue;
        }
        let routes = Generation::from_native(plan.routes())?;
        retiring.insert(
            gateway.generation().number().get(),
            Retiring {
                gateway: gateway.clone(),
                routes,
            },
        );
    }
    let issuer = serving
        .as_ref()
        .ok_or(Error::ControllerReadiness)?
        .issuer
        .clone();
    let mut completed = Vec::new();
    for (number, old) in retiring.iter() {
        if old.routes.connections(Role::Egress) != 0 {
            continue;
        }
        if removed(&old.gateway, authority).await? {
            completed.push(*number);
            continue;
        }
        let gateway = old.gateway.clone();
        let root = authority.to_owned();
        let stopped = tokio::task::spawn_blocking(move || gateway.eligible_stopped(&root))
            .await
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        if !stopped {
            let client = old.gateway.client();
            let status = client.drain(true).await.map_err(std::io::Error::other)?;
            if status.connections != 0 {
                continue;
            }
            issuer
                .release_drained(&client)
                .await
                .map_err(std::io::Error::other)?;
            old.gateway
                .record_eligible(authority)
                .map_err(std::io::Error::other)?;
        }
        let gateway = old.gateway.clone();
        let deployment = plan.deployment().clone();
        tokio::task::spawn_blocking(move || gateway.retire(&deployment))
            .await
            .map_err(std::io::Error::other)?
            .map_err(std::io::Error::other)?;
        old.gateway
            .record_removed(authority)
            .map_err(std::io::Error::other)?;
        completed.push(*number);
    }
    for number in completed {
        retiring.remove(&number);
    }
    publish(
        authority,
        plan,
        if retiring.is_empty() && plan.backend_open() {
            State::Activated
        } else {
            State::Draining
        },
        retiring.len(),
    )
    .await?;
    Ok(())
}

// Native pool initialization can saturate the same filesystem. Durable core
// status still precedes acknowledgment, but its fsync must not stall private
// admission IPC and turn a healthy queued preparation into a client timeout.
async fn publish(
    authority: &Path,
    plan: &Plan,
    state: State,
    retiring: usize,
) -> Result<(), Error> {
    let authority = authority.to_owned();
    let plan = plan.clone();
    tokio::task::spawn_blocking(move || Status::publish(&authority, &plan, state, retiring))
        .await
        .map_err(std::io::Error::other)?
        .map_err(|error| std::io::Error::other(error).into())
}
