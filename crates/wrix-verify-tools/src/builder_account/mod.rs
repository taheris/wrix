//! Check the builder image's dedicated unprivileged build-user pool.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// builder account input I/O failed: {0}
    Io(#[from] std::io::Error),
    /// builder account contract failed: {0}
    Contract(&'static str),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Name(String);
impl Name {
    fn parse(value: &str) -> Result<Self, Error> {
        if value.is_empty() || value.contains([':', '\n', '\r', ',']) {
            return Err(Error::Contract("invalid account name"));
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Id(u32);
impl Id {
    fn parse(value: &str) -> Result<Self, Error> {
        value
            .parse()
            .map(Self)
            .map_err(|_| Error::Contract("invalid account ID"))
    }
}
struct User {
    uid: Id,
    gid: Id,
    home: String,
    shell: String,
}
struct Group {
    gid: Id,
    members: Vec<Name>,
}
const fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Contract(message))
    }
}

fn users(text: &str) -> Result<BTreeMap<Name, User>, Error> {
    let mut users = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        require(fields.len() == 7, "passwd record shape")?;
        let uid = Id::parse(fields[2])?;
        require(ids.insert(uid), "duplicate UIDs")?;
        let user = User {
            uid,
            gid: Id::parse(fields[3])?,
            home: fields[5].to_owned(),
            shell: fields[6].to_owned(),
        };
        require(
            users.insert(Name::parse(fields[0])?, user).is_none(),
            "duplicate user names",
        )?;
    }
    Ok(users)
}
fn groups(text: &str) -> Result<BTreeMap<Name, Group>, Error> {
    let mut groups = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for line in text.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        require(fields.len() == 4, "group record shape")?;
        let gid = Id::parse(fields[2])?;
        require(ids.insert(gid), "duplicate GIDs")?;
        let members = if fields[3].is_empty() {
            Vec::new()
        } else {
            fields[3]
                .split(',')
                .map(Name::parse)
                .collect::<Result<_, _>>()?
        };
        require(
            groups
                .insert(Name::parse(fields[0])?, Group { gid, members })
                .is_none(),
            "duplicate group names",
        )?;
    }
    Ok(groups)
}

fn inspect(passwd: &str, group: &str, configured: &str, trusted: &str) -> Result<(), Error> {
    let users = users(passwd)?;
    let groups = groups(group)?;
    let group = groups
        .get(&Name::parse(configured)?)
        .ok_or(Error::Contract("configured build group missing from image"))?;
    let members = group.members.iter().collect::<BTreeSet<_>>();
    require(
        group.members.len() == 32 && members.len() == 32,
        "expected 32 distinct build users",
    )?;
    require(
        !group
            .members
            .iter()
            .any(|name| matches!(name.0.as_str(), "root" | "builder" | "sshd" | "nobody")),
        "service account in build pool",
    )?;
    require(
        trusted.split_whitespace().collect::<BTreeSet<_>>() == BTreeSet::from(["root", "builder"]),
        "build users must not be trusted Nix clients",
    )?;
    for member in members {
        let user = users
            .get(member)
            .ok_or(Error::Contract("build user missing from passwd"))?;
        require(
            (30001..=30032).contains(&user.uid.0),
            "build UID outside dedicated range",
        )?;
        require(
            user.gid == group.gid && group.gid == Id(30000),
            "build user's primary group is inconsistent",
        )?;
        require(
            user.home == "/var/empty" && user.shell == "/bin/false",
            "build account allows interactive login",
        )?;
    }
    Ok(())
}

pub fn run(passwd: &Path, group: &Path, configured: &str, trusted: &str) -> Result<(), Error> {
    inspect(
        &fs::read_to_string(passwd)?,
        &fs::read_to_string(group)?,
        configured,
        trusted,
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fmt::Write;
    fn fixture() -> (String, String) {
        let mut passwd = String::new();
        for id in 1..=32 {
            writeln!(
                passwd,
                "build{id}:x:{}:30000::/var/empty:/bin/false",
                30000 + id
            )
            .unwrap();
        }
        let members = (1..=32)
            .map(|id| format!("build{id}"))
            .collect::<Vec<_>>()
            .join(",");
        (passwd, format!("nixbld:x:30000:{members}\n"))
    }
    #[test]
    fn all_original_builder_pool_assertions_reject_corruption() {
        let (passwd, group) = fixture();
        inspect(&passwd, &group, "nixbld", "root builder").unwrap();
        for (passwd, group, configured, trusted) in [
            (
                passwd.replace("build2:", "build1:"),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.replace(":30002:", ":30001:"),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                format!("{group}{group}"),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                format!("{group}other:x:30000:\n"),
                "nixbld",
                "root builder",
            ),
            (passwd.clone(), group.clone(), "missing", "root builder"),
            (
                passwd.clone(),
                group.replace(",build32", ""),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                group.replace(",build32", ",build31"),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                group.replace(",build32", ",root"),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                group.clone(),
                "nixbld",
                "root builder build1",
            ),
            (
                passwd.replacen("build1:", "missing:", 1),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.replace(":30001:", ":40001:"),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.replace(":30000:", ":30001:"),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.clone(),
                group.replace(":30000:", ":30001:"),
                "nixbld",
                "root builder",
            ),
            (
                passwd.replace("/var/empty", "/root"),
                group.clone(),
                "nixbld",
                "root builder",
            ),
            (
                passwd.replace("/bin/false", "/bin/bash"),
                group,
                "nixbld",
                "root builder",
            ),
        ] {
            assert!(inspect(&passwd, &group, configured, trusted).is_err());
        }
    }
    #[test]
    fn malformed_records_fail_closed() {
        let (passwd, group) = fixture();
        for wrong in ["", "broken", "name:x:bad:30000::/var/empty:/bin/false"] {
            assert!(inspect(wrong, &group, "nixbld", "root builder").is_err());
        }
        assert!(inspect(&passwd, "nixbld:x:bad:", "nixbld", "root builder").is_err());
    }
}
