//! Bounded controller-owned preparation. Neither queued nor ready grants a worker or proves exit.
use super::{Controller, Error, Namespace, Reply};
use crate::nix_store::{
    image::Candidate,
    owner::{
        Reservation,
        backend::{self, tools::Tools},
    },
};
use std::{
    collections::BTreeMap,
    fs, io,
    sync::{Arc, Mutex, Weak},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use wrix_core::nix_store::pool::Key;
use wrix_state::nix_store::admission::request::{self, Accepted, Id, Purpose};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Pool,
    Execution,
    Worker,
}
enum Preparation {
    Pool,
    Execution,
    Worker(backend::seed::Image),
}

const LIMIT: usize = 128;
const ACQUISITION: Duration = Duration::from_secs(120);

/// Historical preparation-job status, never current native readiness, worker admission or exit evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Ready,
    Failed,
    Unknown,
}

enum Prepared {
    Pool,
    Execution(backend::client::Delivery),
}
enum Outcome {
    Ready(Prepared),
    Failed,
}
enum Job {
    Running(JoinHandle<Result<Prepared, Error>>),
    Finished(Outcome),
}
#[derive(Default)]
struct Coordination(Mutex<Vec<(Key, Weak<Mutex<()>>)>>);
impl Coordination {
    fn window(&self, pool: &Key) -> Result<Arc<Mutex<()>>, Error> {
        let mut windows = self.0.lock().map_err(|_| Error::LockPoisoned)?;
        windows.retain(|(_, window)| window.strong_count() != 0);
        if let Some(window) = windows
            .iter()
            .find(|(key, _)| key == pool)
            .and_then(|(_, window)| window.upgrade())
        {
            return Ok(window);
        }
        let window = Arc::new(Mutex::new(()));
        windows.push((pool.clone(), Arc::downgrade(&window)));
        Ok(window)
    }
}
#[derive(Default)]
pub(super) struct Queue {
    jobs: Mutex<BTreeMap<Id, Job>>,
    coordination: Arc<Coordination>,
    supervisor: Arc<backend::client::supervisor::Supervisor>,
}

impl Queue {
    pub(super) fn recover_startup(&self, controller: &Controller) {
        let host = match wrix_state::host::Host::discover() {
            Ok(host) => host,
            Err(error) => {
                tracing::debug!(operation = "nix_execution_recover", %error, "host pool supervision is unavailable; protection is not released");
                return;
            }
        };
        let address = match controller.listener.local_addr() {
            Ok(address) => address,
            Err(error) => {
                tracing::warn!(operation = "nix_execution_recover", %error, "controller identity is unavailable; protection is retained");
                return;
            }
        };
        if address.as_pathname()
            == Some(
                host.roots()
                    .state()
                    .join("control/authority/control.sock")
                    .as_path(),
            )
        {
            let supervisor = self.supervisor.clone();
            let deployment = controller.deployment.clone();
            if let Err(error) = thread::Builder::new().name("wrix-pool-recover".to_owned()).spawn(move || {
                if let Err(error) = supervisor.recover_all(&Arc::new(host), &deployment) { tracing::warn!(operation = "nix_execution_recover", %error, "recorded pool supervision is uncertain; protection is retained"); }
            }) { tracing::warn!(operation = "nix_execution_recover", %error, "pool supervision could not start; protection is retained"); }
        }
    }

    pub(super) fn prepare(
        &self,
        controller: &Controller,
        id: Id,
        action: Action,
    ) -> Result<Reply, Error> {
        let host = wrix_state::host::Host::discover().map_err(io::Error::other)?;
        let root = host.roots().state().join("control/authority");
        if controller.listener.local_addr()?.as_pathname()
            != Some(root.join("control.sock").as_path())
        {
            return Err(Error::PrivateRoot);
        }
        let mut jobs = self.jobs.lock().map_err(|_| Error::LockPoisoned)?;
        // Retrying an accepted ID refers to the same job; it never starts another VM or grants a client.
        if jobs.contains_key(&id) {
            return Ok(Reply::PoolQueued { id });
        }
        finish(&mut jobs);
        if jobs.len() >= LIMIT {
            jobs.retain(|_, job| matches!(job, Job::Running(_)));
        }
        if jobs.len() >= LIMIT {
            return Ok(Reply::Capacity);
        }
        let candidate = Candidate::from_env().map_err(backend::Error::from)?;
        let tools = Tools::from_env()?;
        let deployment = controller.deployment.clone();
        let coordination = self.coordination.clone();
        let supervisor = self.supervisor.clone();
        let request = id.clone();
        let handle = thread::Builder::new()
            .name("wrix-pool-prepare".to_owned())
            .spawn(move || {
                // Scope re-attestation can invoke the bounded host resolver; never run it in the control loop.
                let accepted = request::claim(&request)?;
                if !matches!(
                    (action, accepted.purpose()),
                    (Action::Pool, Purpose::Preparation)
                        | (Action::Execution, Purpose::Execution { .. })
                        | (Action::Worker, Purpose::Worker { .. })
                ) {
                    return Err(Error::PermissionDenied);
                }
                if matches!(action, Action::Execution | Action::Worker) {
                    let plane = match accepted.purpose() {
                        Purpose::Worker { plane, .. } => *plane,
                        _ => request::Plane::Host,
                    };
                    tools.view_check(plane)?;
                }
                let namespace = Namespace::from(accepted.namespace());
                let registrations = super::registration::Store::open(&root)?;
                {
                    let _lease = registrations.lease(&namespace)?;
                    let mut namespaces = registrations.lock()?;
                    if !namespaces.contains_key(&namespace) {
                        if !matches!(accepted.purpose(), Purpose::Worker { .. }) {
                            return Err(Error::UnknownNamespace);
                        }
                        // Explicit checked shared launch admits only its pool, never Celler/SQL/proxy/compiler access.
                        accepted.recheck()?;
                        if namespaces.len() >= super::MAX_NAMESPACES {
                            return Err(Error::PoolRequest);
                        }
                        namespaces.insert(
                            namespace,
                            super::Policy {
                                dolt: super::Access::Denied,
                                private_cache_read: super::Access::Denied,
                                common_cache_read: super::Access::Denied,
                                egress: super::Access::Denied,
                                compiler: super::Access::Denied,
                            },
                        );
                        namespaces.commit()?;
                    }
                }
                let preparation = match accepted.purpose() {
                    Purpose::Preparation => Preparation::Pool,
                    Purpose::Execution { .. } => Preparation::Execution,
                    Purpose::Worker { image, .. } => {
                        let digest = wrix_sandbox::image::Digest::parse(image.digest().as_str())
                            .map_err(|_| Error::PoolRequest)?;
                        Preparation::Worker(backend::seed::Image::inspect(
                            accepted.pool().system,
                            &digest,
                            image.metadata(),
                        )?)
                    }
                };
                let window = coordination.window(accepted.pool())?;
                let _window = window.lock().map_err(|_| Error::LockPoisoned)?;
                let deadline = Instant::now() + ACQUISITION;
                loop {
                    match ensure(&accepted, &deployment, &candidate, &tools) {
                        Err(backend::Error::Busy) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Ok(mut owner) => {
                            supervisor.recover(&accepted.host_handle(), &deployment, &owner)?;
                            return match &preparation {
                                Preparation::Pool => Ok(Prepared::Pool),
                                Preparation::Execution | Preparation::Worker(_) => {
                                    if let Preparation::Worker(image) = &preparation {
                                        owner.seed_execution(&accepted, image)?;
                                    }
                                    let delivery = owner.admit_execution(&accepted)?;
                                    supervisor.start(
                                        accepted.host_handle(),
                                        deployment.clone(),
                                        accepted.namespace().clone(),
                                        &delivery,
                                    )?;
                                    Ok(Prepared::Execution(delivery))
                                }
                            };
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            })?;
        jobs.insert(id.clone(), Job::Running(handle));
        Ok(Reply::PoolQueued { id })
    }

    pub(super) fn delivery(&self, id: &Id) -> Result<Reply, Error> {
        let mut jobs = self.jobs.lock().map_err(|_| Error::LockPoisoned)?;
        finish(&mut jobs);
        Ok(match jobs.get(id) {
            Some(Job::Finished(Outcome::Ready(Prepared::Execution(delivery)))) => {
                Reply::PoolDelivery {
                    delivery: delivery.clone(),
                }
            }
            Some(Job::Running(_)) => Reply::PoolPreparation {
                status: Status::Pending,
            },
            Some(Job::Finished(Outcome::Failed)) => Reply::PoolPreparation {
                status: Status::Failed,
            },
            _ => Reply::PoolPreparation {
                status: Status::Unknown,
            },
        })
    }

    pub(super) fn inspect(&self, id: &Id) -> Result<Reply, Error> {
        let mut jobs = self.jobs.lock().map_err(|_| Error::LockPoisoned)?;
        finish(&mut jobs);
        let status = match jobs.get(id) {
            Some(Job::Running(_)) => Status::Pending,
            Some(Job::Finished(Outcome::Ready(_))) => Status::Ready,
            Some(Job::Finished(Outcome::Failed)) => Status::Failed,
            None => Status::Unknown,
        };
        Ok(Reply::PoolPreparation { status })
    }
}

fn finish(jobs: &mut BTreeMap<Id, Job>) {
    let finished: Vec<Id> = jobs
        .iter()
        .filter_map(|(id, job)| match job {
            Job::Running(handle) if handle.is_finished() => Some(id.clone()),
            _ => None,
        })
        .collect();
    for id in finished {
        if let Some(Job::Running(handle)) = jobs.remove(&id) {
            let status = match handle.join() {
                Ok(Ok(prepared)) => Outcome::Ready(prepared),
                Ok(Err(error)) => {
                    tracing::warn!(operation = "nix_pool_prepare", %error, "pool preparation failed; retained ownership is not released");
                    Outcome::Failed
                }
                Err(_) => {
                    tracing::warn!(
                        operation = "nix_pool_prepare",
                        "pool preparation panicked; retained ownership requires reconciliation"
                    );
                    Outcome::Failed
                }
            };
            jobs.insert(id, Job::Finished(status));
        }
    }
}

fn ensure(
    accepted: &Accepted,
    deployment: &crate::deployment::Id,
    candidate: &Candidate,
    tools: &Tools,
) -> Result<backend::Owner, backend::Error> {
    let host = accepted.host();
    accepted.recheck().map_err(io::Error::other)?;
    let pool = accepted.pool();
    let root = host
        .roots()
        .state()
        .join("control/nix")
        .join(pool.repository.to_string())
        .join(pool.system.as_str());
    match fs::symlink_metadata(root) {
        Ok(_) => {
            let mut owner = backend::Owner::open(host, deployment, pool)?;
            owner.ensure_ready()?;
            Ok(owner)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tools.check(pool.system)?;
            Reservation::reserve(host, deployment, pool)?
                .prepare(tools)?
                .start(candidate.clone(), tools.clone())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn preparation_protocol_accepts_only_request_identifiers_not_pool_or_path_authority() {
        let id = "a".repeat(32);
        assert!(
            serde_json::from_value::<super::super::Operation>(
                serde_json::json!({"operation":"prepare_nix_pool","id":id})
            )
            .is_ok()
        );
        for value in [
            serde_json::json!({"operation":"prepare_nix_pool","namespace":"a".repeat(64)}),
            serde_json::json!({"operation":"prepare_nix_pool","id":id,"pool":"peer"}),
            serde_json::json!({"operation":"prepare_nix_pool","id":id,"path":"/nix/store"}),
            serde_json::json!({"operation":"prepare_nix_pool","id":"../scope"}),
        ] {
            assert!(serde_json::from_value::<super::super::Operation>(value).is_err());
        }
    }

    #[tokio::test]
    async fn unknown_preparation_status_does_not_probe_or_prepare_a_native_pool() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap().join("authority");
        let controller = Controller::bind(&root).unwrap();
        let client = super::super::Client::open(&root).unwrap();
        let id = Id::try_from("a".repeat(32)).unwrap();
        let (reply, served) = tokio::join!(
            client.request(super::super::Operation::InspectNixPoolPreparation { id }),
            controller.serve_one()
        );
        served.unwrap();
        assert_eq!(
            reply.unwrap(),
            Reply::PoolPreparation {
                status: Status::Unknown
            }
        );
        assert!(!root.parent().unwrap().join("nix").exists());
        controller.close().unwrap();
    }

    #[test]
    fn concurrent_jobs_serialize_only_the_same_pool() {
        let first = Key {
            repository: "a".repeat(32).try_into().unwrap(),
            system: wrix_core::nix_store::pool::System::X86_64Linux,
        };
        let second = Key {
            repository: "b".repeat(32).try_into().unwrap(),
            system: first.system,
        };
        let coordination = Coordination::default();
        let window = coordination.window(&first).unwrap();
        assert!(Arc::ptr_eq(&window, &coordination.window(&first).unwrap()));
        assert!(!Arc::ptr_eq(
            &window,
            &coordination.window(&second).unwrap()
        ));
        let guard = window.lock().unwrap();
        assert!(coordination.window(&first).unwrap().try_lock().is_err());
        assert!(coordination.window(&second).unwrap().try_lock().is_ok());
        drop(guard);
        assert!(window.try_lock().is_ok());
    }
}
