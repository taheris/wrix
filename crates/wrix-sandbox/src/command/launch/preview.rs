//! Disclosure-limited launch inspection; no preparation or runtime execution.

use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use serde::Serialize;
use wrix_core::repository_policy::Policy;

use super::{
    AgentKind, GitGrants, KeyKind, Kind, LaunchError, MountMode, Plan, Platform,
    RuntimeSecretPolicy, deploy_key_name, expand_path, home_dir, resolve_key,
};

#[derive(Clone, Copy, Default)]
pub struct Options {
    pub dry_run: bool,
    pub json: bool,
}

impl Options {
    pub(in crate::command) fn from_args(command: super::super::Command, args: &[String]) -> Self {
        let mut options = Self::default();
        let mut skip_value = false;
        for arg in args {
            if skip_value {
                skip_value = false;
                continue;
            }
            match arg.as_str() {
                "--" => break,
                "--spawn-config" | "--profile-config" => skip_value = true,
                "--dry-run" => options.dry_run = true,
                "--json" => options.json = true,
                value if !value.starts_with('-') && command == super::super::Command::Run => break,
                _ => (),
            }
        }
        options
    }
    pub const fn validate(self) -> Result<(), super::super::CliError> {
        if self.json && !self.dry_run {
            return Err(super::super::CliError::JsonRequiresPreview);
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Origin {
    Invocation,
    Repository,
    Default,
    HostEnvironment,
    SpawnEnvironment,
    ManagedFile,
}

#[derive(Serialize)]
struct Grant {
    enabled: bool,
    origin: Origin,
}

impl Grant {
    fn resolve(explicit: Option<bool>, repository: Option<bool>) -> Self {
        Self {
            enabled: explicit.or(repository).unwrap_or(false),
            origin: if explicit.is_some() {
                Origin::Invocation
            } else if repository.is_some() {
                Origin::Repository
            } else {
                Origin::Default
            },
        }
    }
}

#[derive(Serialize)]
struct Credential {
    name: String,
    source: Origin,
    available: bool,
}

#[derive(Serialize)]
struct Mount {
    source: PathBuf,
    destination: String,
    read_only: bool,
    optional: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Code {
    InvalidInput,
    RestrictedRustSharedCaches,
    CredentialMissing,
    PiAuthInitialization,
    PreparationRequired,
    EnforcementUnverified,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Blocker,
    Preparation,
    Information,
}

#[derive(Serialize)]
struct Finding {
    code: Code,
    severity: Severity,
    message: &'static str,
}

#[derive(Serialize)]
struct Summary<'a> {
    profile: String,
    agent: AgentKind,
    profile_config: &'a Path,
    workspace: &'a Path,
    image_ref: &'a str,
    image_source: &'a Path,
    image_source_kind: &'a str,
    image_digest: Option<String>,
    runtime: Runtime,
    isolation: Isolation,
    mounts: Vec<Mount>,
    network: &'a str,
    allowlist: &'a [super::super::config::NetworkRule],
    cpus: Option<u32>,
    memory_mb: u32,
    pids_limit: u32,
    git_deploy: Grant,
    git_sign: Grant,
    tool_cache_mode: &'a str,
    credentials: Vec<Credential>,
    service_endpoints: Option<()>,
    preparation_mounts: Option<()>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Runtime {
    Podman,
    AppleContainer,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Isolation {
    RootlessContainer,
    Microvm,
}

#[derive(Serialize)]
pub(in crate::command) struct Report<'a> {
    schema: u8,
    plan: Option<Summary<'a>>,
    findings: Vec<Finding>,
}

impl<'a> Report<'a> {
    pub(in crate::command) fn invalid() -> Self {
        Self {
            schema: 1,
            plan: None,
            findings: vec![Finding {
                code: Code::InvalidInput,
                severity: Severity::Blocker,
                message: "Invalid launch input; check selected configuration and launcher options (values omitted).",
            }],
        }
    }

    pub(super) fn resolved(
        plan: &'a Plan<'_>,
        policy: &Policy,
        repository: Option<&Path>,
        conflict: bool,
    ) -> Result<Self, LaunchError> {
        let request = plan.request;
        let profile = &request.profile_config;
        let overrides = match &request.kind {
            Kind::Run(run) => GitGrants {
                deploy: run.git_deploy,
                sign: run.git_sign,
            },
            Kind::Spawn(spawn) => spawn.config.git,
        };
        let deploy = Grant::resolve(overrides.deploy, policy.git.deploy);
        let sign = Grant::resolve(overrides.sign, policy.git.sign);
        let mut credentials = Vec::new();
        let mut findings = Vec::new();
        if conflict {
            findings.push(Finding { code: Code::RestrictedRustSharedCaches, severity: Severity::Blocker, message: "Restricted Rust requires private tool caches: use --no-cache or tool_caches.shared = false; Nix/Celler remains enabled." });
        }
        if deploy.enabled || sign.enabled {
            let name = deploy_key_name(
                repository.unwrap_or(&plan.workspace),
                policy
                    .git
                    .deploy_key
                    .as_ref()
                    .or(profile.security.deploy_key.as_ref()),
            )?;
            for (enabled, kind, variable) in [
                (deploy.enabled, KeyKind::Deploy, "WRIX_DEPLOY_KEY"),
                (sign.enabled, KeyKind::Signing, "WRIX_SIGNING_KEY"),
            ] {
                if enabled {
                    credentials.push(Credential {
                        name: variable.to_owned(),
                        source: if env::var_os(variable).is_some() {
                            Origin::HostEnvironment
                        } else {
                            Origin::ManagedFile
                        },
                        available: resolve_key(kind, &name).is_ok(),
                    });
                }
            }
        }
        let missing_git_key = credentials.iter().any(|credential| !credential.available);
        for (name, requirement) in &profile.security.runtime_secrets {
            let supplied = match &request.kind {
                Kind::Run(_) => false,
                Kind::Spawn(spawn) => spawn.config.env.iter().any(|(key, _)| key == name),
            };
            let available = supplied || env::var_os(name.as_str()).is_some();
            credentials.push(Credential {
                name: name.to_string(),
                source: if supplied {
                    Origin::SpawnEnvironment
                } else {
                    Origin::HostEnvironment
                },
                available,
            });
            if !available && *requirement == RuntimeSecretPolicy::Required {
                findings.push(Finding {
                    code: Code::CredentialMissing,
                    severity: Severity::Blocker,
                    message: "A required runtime credential is unavailable.",
                });
            }
        }
        if missing_git_key {
            findings.push(Finding {
                code: Code::CredentialMissing,
                severity: Severity::Blocker,
                message: "A granted Git key is unavailable.",
            });
        }
        if profile.agent.kind == AgentKind::Pi {
            let explicit = env::var_os("WRIX_PI_AUTH_FILE");
            let path = explicit
                .as_ref()
                .map_or_else(|| home_dir().join(".pi/agent/auth.json"), PathBuf::from);
            let available = path.is_file();
            credentials.push(Credential {
                name: "Pi auth.json".to_owned(),
                source: if explicit.is_some() {
                    Origin::HostEnvironment
                } else {
                    Origin::ManagedFile
                },
                available,
            });
            if !available {
                let initialize = explicit.is_none() && !plan.spawn();
                findings.push(Finding { code: if initialize { Code::PiAuthInitialization } else { Code::CredentialMissing }, severity: if initialize { Severity::Preparation } else { Severity::Blocker }, message: "Pi credential storage needs initialization or an existing selected auth file." });
            }
        }
        let mut mounts = Vec::new();
        for mount in &profile.profile.mounts {
            mounts.push(Mount {
                source: expand_path(&mount.source)?,
                destination: mount.dest.clone(),
                read_only: mount.mode == MountMode::Ro,
                optional: mount.optional,
            });
        }
        for mount in &plan.spawn_mounts {
            mounts.push(Mount {
                source: PathBuf::from(&mount.host),
                destination: mount.container.clone(),
                read_only: mount.mode == MountMode::Ro,
                optional: mount.optional,
            });
        }
        findings.push(Finding { code: Code::PreparationRequired, severity: Severity::Preparation, message: "Actual launch rechecks prerequisites and prepares required services, grants, mounts and image presence; unallocated endpoints and paths remain unresolved." });
        findings.push(Finding { code: Code::EnforcementUnverified, severity: Severity::Information, message: "Guest enforcement and workload execution remain unverified; preview does not guarantee launch success." });
        Ok(Self {
            schema: 1,
            plan: Some(Summary {
                profile: profile.profile.name.to_string(),
                agent: profile.agent.kind,
                profile_config: &request.profile_config_path,
                workspace: &plan.workspace,
                image_ref: plan.image_ref.as_str(),
                image_source: plan.image_source.path(),
                image_source_kind: plan.image_source.kind().as_str(),
                image_digest: plan
                    .image_digest
                    .as_ref()
                    .map(|digest| digest.as_str().to_owned()),
                runtime: if Platform::CURRENT == Platform::Linux {
                    Runtime::Podman
                } else {
                    Runtime::AppleContainer
                },
                isolation: if Platform::CURRENT == Platform::Darwin
                    || super::env_flag("WRIX_MICROVM")
                {
                    Isolation::Microvm
                } else {
                    Isolation::RootlessContainer
                },
                mounts,
                network: plan.network_mode.as_str(),
                allowlist: &profile.profile.network_allowlist,
                cpus: profile.resources.cpus,
                memory_mb: profile.resources.memory_mb,
                pids_limit: profile.resources.pids_limit,
                git_deploy: deploy,
                git_sign: sign,
                tool_cache_mode: plan.tool_cache_mode.as_str(),
                credentials,
                service_endpoints: None,
                preparation_mounts: None,
            }),
            findings,
        })
    }

    pub(in crate::command) fn write(
        &self,
        stdout: &mut impl Write,
        json: bool,
    ) -> io::Result<ExitCode> {
        if json {
            serde_json::to_writer(&mut *stdout, self).map_err(io::Error::other)?;
            writeln!(stdout)?;
        } else {
            writeln!(stdout, "Launch preview (schema {})", self.schema)?;
            if let Some(plan) = &self.plan {
                // Serialization contains only the disclosure-limited summary, never the launch request.
                let summary = serde_json::to_value(plan).map_err(io::Error::other)?;
                if let serde_json::Value::Object(fields) = summary {
                    for (key, value) in fields {
                        writeln!(stdout, "{key}: {value}")?;
                    }
                }
            }
            for finding in &self.findings {
                writeln!(
                    stdout,
                    "{}: {}",
                    serde_json::to_value(&finding.code).map_err(io::Error::other)?,
                    finding.message
                )?;
            }
        }
        Ok(
            if self
                .findings
                .iter()
                .any(|finding| matches!(finding.severity, Severity::Blocker))
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            },
        )
    }
}
