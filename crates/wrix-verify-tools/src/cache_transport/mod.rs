use std::{fmt, fs, io, path::Path};

#[derive(Clone, Copy, Debug)]
pub enum Region {
    Launcher,
    Owner,
    Server,
    Image,
}
impl Region {
    const fn path(self) -> &'static str {
        match self {
            Self::Launcher => "crates/wrix-sandbox/src/command/launch.rs",
            Self::Owner => "crates/wrix-service/src/lifecycle/owner/host.rs",
            Self::Server => "crates/wrix-service/src/dolt/server/mod.rs",
            Self::Image => "lib/services/image.nix",
        }
    }
    const fn markers(self) -> (&'static str, &'static str) {
        match self {
            Self::Launcher => ("    fn load_project_cache", "    fn load_dolt"),
            Self::Owner => (
                "pub(in crate::lifecycle) fn ensure(",
                "\npub(in crate::lifecycle) fn sql_ready(",
            ),
            Self::Server => ("    let mut celler =", "    let mut terminate ="),
            Self::Image => ("  stableContents = [", "  sourceKind ="),
        }
    }
    const fn required(self) -> &'static [&'static str] {
        match self {
            Self::Launcher => &[
                "format!(\"http://{sandbox_host}:{}\", endpoint.port)",
                "extra-substituters = {url}",
                "extra-trusted-public-keys = {}",
                "public_key.as_str()",
                "builders-use-substitutes = true",
            ],
            Self::Owner => &[
                "\"127.0.0.1:{}:8080\",\n            shared.cache_port(selected.port)",
                "let layout = Layout::current()?",
                "for mount in layout.mounts()",
                ".arg(\"wrix-shared-service\")",
            ],
            Self::Server => &["wrix_cache::backend::layout::runtime::COMMAND"],
            Self::Image => &["celler"],
        }
    }
}
impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Launcher => "project-cache launcher path",
            Self::Owner => "service container run path",
            Self::Server => "service container command",
            Self::Image => "service image contents",
        })
    }
}
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// FAIL: source audit I/O failed: {0}
    Io(#[from] io::Error),
    /// FAIL: {region}: missing start marker {marker}
    Start { region: Region, marker: Quoted },
    /// FAIL: {region}: missing end marker {marker}
    End { region: Region, marker: Quoted },
    /// FAIL: {region}: missing {needle}
    Missing { region: Region, needle: Quoted },
    /// FAIL: {region}: unexpected {needle}
    Forbidden { region: Region, needle: Quoted },
}
#[derive(Debug)]
pub struct Quoted(String);
impl Quoted {
    fn new(text: &str) -> Self {
        let quote = if text.contains('\'') && !text.contains('"') {
            '"'
        } else {
            '\''
        };
        let mut result = String::new();
        result.push(quote);
        for character in text.chars() {
            match character {
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                value if value == quote => {
                    result.push('\\');
                    result.push(value);
                }
                value => result.push(value),
            }
        }
        result.push(quote);
        Self(result)
    }
}
impl fmt::Display for Quoted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

const REGIONS: [Region; 4] = [
    Region::Launcher,
    Region::Owner,
    Region::Server,
    Region::Image,
];
const FORBIDDEN: [&str; 7] = [
    "file://",
    "unix://",
    "nix-daemon",
    "/nix/var/nix/daemon-socket",
    ":/nix/store",
    "harmonia",
    "nix-serve",
];
fn section(region: Region, source: &str) -> Result<&str, Error> {
    let (start, end) = region.markers();
    let begin = source.find(start).ok_or_else(|| Error::Start {
        region,
        marker: Quoted::new(start),
    })?;
    let offset = begin + start.len();
    let end = source[offset..].find(end).ok_or_else(|| Error::End {
        region,
        marker: Quoted::new(end),
    })? + offset;
    Ok(&source[begin..end])
}
fn check(sources: &[String; 4]) -> Result<(), Error> {
    let mut sections = Vec::new();
    for (region, source) in REGIONS.into_iter().zip(sources) {
        sections.push((region, section(region, source)?));
    }
    for (region, source) in &sections {
        for needle in region.required() {
            if !source.contains(needle) {
                return Err(Error::Missing {
                    region: *region,
                    needle: Quoted::new(needle),
                });
            }
        }
    }
    for (region, source) in sections {
        for needle in FORBIDDEN {
            if source.contains(needle) {
                return Err(Error::Forbidden {
                    region,
                    needle: Quoted::new(needle),
                });
            }
        }
    }
    Ok(())
}
pub fn run(root: &Path) -> Result<(), Error> {
    let sources = REGIONS.map(|region| fs::read_to_string(root.join(region.path())));
    let [launcher, owner, server, image] = sources;
    check(&[launcher?, owner?, server?, image?])
}
#[cfg(test)]
mod fixture;
#[cfg(test)]
mod test;
