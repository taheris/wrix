//! Launcher-owned native credentials; no token environment, detached renewer or issuer authority.

use std::{
    fs::{self, File},
    io::{self, Write},
    net::IpAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    thread,
    time::{Duration, Instant, SystemTime},
};

use displaydoc::Display;
use thiserror::Error as ThisError;

pub const GUEST_DIRECTORY: &str = "/run/wrix/cache-credentials";
pub const GUEST_NETRC: &str = "/run/wrix/cache-credentials/netrc";
const ADVANCE: Duration = Duration::from_secs(300);
const RETRY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(1);
const MAX_BEARER: usize = 16 * 1024;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// host cache credential issuance is unavailable
    Unavailable,
    /// host cache credential is malformed
    Credential,
    /// host cache credential has already expired
    Expired,
    /// private cache credential staging failed
    Staging(#[source] io::Error),
}

/// Opaque runtime-only native bearer and its original absolute expiry.
pub struct Grant {
    bearer: String,
    expires_at: SystemTime,
}

impl Grant {
    pub fn new(bearer: String, expires_at: SystemTime) -> Result<Self, Error> {
        if bearer.is_empty()
            || bearer.len() > MAX_BEARER
            || !bearer
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(Error::Credential);
        }
        Ok(Self { bearer, expires_at })
    }

    fn renewal_at(&self, now: SystemTime) -> Result<SystemTime, Error> {
        if self.expires_at <= now {
            return Err(Error::Expired);
        }
        let remaining = self
            .expires_at
            .duration_since(now)
            .map_err(|_| Error::Expired)?;
        self.expires_at
            .checked_sub(ADVANCE.min(remaining / 10))
            .ok_or(Error::Credential)
    }
}

/// Implementations issue from current host authorization and bound each request's lifetime.
pub trait Issuer {
    fn issue(&mut self) -> Result<Grant, Error>;
}

#[derive(Clone, Copy)]
pub struct Time {
    pub wall: SystemTime,
    pub monotonic: Duration,
}

pub trait Clock {
    fn now(&self) -> Time;
}

pub struct NativeClock(Instant);

impl Default for NativeClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for NativeClock {
    fn now(&self) -> Time {
        Time {
            wall: SystemTime::now(),
            monotonic: self.0.elapsed(),
        }
    }
}

struct Attempt {
    wall: SystemTime,
    monotonic: Duration,
}

impl Attempt {
    fn retry_due(&self, now: Time) -> bool {
        now.monotonic.saturating_sub(self.monotonic) >= RETRY
            || now
                .wall
                .duration_since(self.wall)
                .is_ok_and(|elapsed| elapsed >= RETRY)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Renewal {
    Unchanged,
    Replaced,
    Retrying,
}

/// One launch's private parent and stable read-only directory-mount source.
pub struct Session<I: Issuer, C: Clock = NativeClock> {
    root: tempfile::TempDir,
    issuer: I,
    clock: C,
    host: IpAddr,
    expires_at: SystemTime,
    renew_at: SystemTime,
    attempted: Option<Attempt>,
}

impl<I: Issuer, C: Clock> Session<I, C> {
    pub fn create(mut issuer: I, clock: C, host: IpAddr) -> Result<Self, Error> {
        let grant = issuer.issue()?;
        let renew_at = grant.renewal_at(clock.now().wall)?;
        let root = tempfile::tempdir().map_err(Error::Staging)?;
        let directory = root.path().join("credentials");
        fs::create_dir(&directory).map_err(Error::Staging)?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))
            .map_err(Error::Staging)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .map_err(Error::Staging)?;
        replace(&directory, host, &grant)?;
        Ok(Self {
            root,
            issuer,
            clock,
            host,
            expires_at: grant.expires_at,
            renew_at,
            attempted: None,
        })
    }

    pub fn directory(&self) -> PathBuf {
        self.root.path().join("credentials")
    }

    pub fn netrc(&self) -> PathBuf {
        self.directory().join("netrc")
    }

    pub const fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    /// Failed renewal leaves the old expiry untouched and never interrupts the runtime.
    pub fn tick(&mut self) -> Renewal {
        let now = self.clock.now();
        if now.wall < self.renew_at
            || self
                .attempted
                .as_ref()
                .is_some_and(|attempt| !attempt.retry_due(now))
        {
            return Renewal::Unchanged;
        }
        match self.renew() {
            Ok(()) => Renewal::Replaced,
            Err(_error) => {
                let now = self.clock.now();
                self.attempted = Some(Attempt {
                    wall: now.wall,
                    monotonic: now.monotonic,
                });
                tracing::warn!(
                    "sandbox cache credential renewal failed; retrying without changing runtime lifetime or extending prior authority"
                );
                Renewal::Retrying
            }
        }
    }

    fn renew(&mut self) -> Result<(), Error> {
        let grant = self.issuer.issue()?;
        let renew_at = grant.renewal_at(self.clock.now().wall)?;
        replace(&self.directory(), self.host, &grant)?;
        self.expires_at = grant.expires_at;
        self.renew_at = renew_at;
        self.attempted = None;
        Ok(())
    }

    /// Completion consumes staging; polling and renewal live only in the waiting caller.
    pub fn wait(mut self, child: &mut Child) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            self.tick();
            thread::park_timeout(POLL);
        }
    }
}

fn replace(directory: &Path, host: IpAddr, grant: &Grant) -> Result<(), Error> {
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(Error::Staging)?;
    writeln!(file, "machine {host} login wrix password {}", grant.bearer)
        .map_err(Error::Staging)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o444))
        .map_err(Error::Staging)?;
    file.as_file().sync_all().map_err(Error::Staging)?;
    file.persist(directory.join("netrc"))
        .map_err(|error| Error::Staging(error.error))?;
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(Error::Staging)?;
    Ok(())
}
