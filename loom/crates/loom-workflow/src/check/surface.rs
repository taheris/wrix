//! `loom check surface` — spec ↔ binary user-facing surface audit (FR13).
//!
//! Four hard-fail dimensions:
//!
//! 1. **Command set** — FR1's bullet lists under `**Workflow** —`,
//!    `**Inspection** —`, `**State** —` ↔ the subcommands the binary
//!    advertises in `loom --help`.
//! 2. **Flag set** — flags declared in the per-command tables
//!    (`### Logs UX`, `### Msg Modes`) ↔ flags `loom <cmd> --help`
//!    renders. Auto-built flags (`-h/--help`, `-V/--version`) and
//!    global flags inherited from the root (`-w/--workspace`,
//!    `-A/--agent`) are filtered out so the audit only sees
//!    per-command surface.
//! 3. **Removed surface** — the `Removed surface` table inside FR1
//!    enumerates commands that must stay absent. Any row whose
//!    `Surface` matches a current top-level subcommand is flagged.
//! 4. **Grouping order** — `Workflow:` / `Inspection:` / `State:`
//!    must appear in FR1's declared order in BOTH `loom --help` and
//!    bare `loom`. The two outputs are compared independently so a
//!    regression on either surface is caught.
//!
//! Help-text *wording* is deliberately out of scope — that's CLI-1
//! style, enforced by `loom review`'s style-rule walk.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("spec file not found at {0}")]
    NoSpec(PathBuf),
    #[error("loom binary not found at {0}")]
    NoBinary(PathBuf),
    #[error("failed to invoke {binary} {args}: {source}")]
    InvokeBinary {
        binary: PathBuf,
        args: String,
        source: std::io::Error,
    },
    #[error("`{cmd}` exited {code}; stderr:\n{stderr}")]
    BinaryFailed {
        cmd: String,
        code: i32,
        stderr: String,
    },
    #[error("malformed spec surface: {0}")]
    MalformedSpec(String),
}

/// One drift the audit detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceFinding {
    pub kind: Drift,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    /// Spec lists a command the binary doesn't advertise.
    MissingCommand,
    /// Binary advertises a command the spec doesn't list under any
    /// FR1 group.
    ExtraCommand,
    /// Spec flag table lists a flag the binary doesn't expose.
    MissingFlag,
    /// Binary exposes a flag the spec's flag table doesn't list.
    ExtraFlag,
    /// `Removed surface` table lists a command that is still in the
    /// binary.
    RemovedSurfacePresent,
    /// `loom --help` group headings are out of order versus FR1.
    GroupingOrderHelp,
    /// Bare `loom` group headings are out of order versus FR1.
    GroupingOrderBare,
    /// `loom --help` and bare `loom` differ in either group order or
    /// the commands listed under each group.
    BareHelpDivergence,
}

impl Drift {
    pub fn tag(&self) -> &'static str {
        match self {
            Drift::MissingCommand => "missing-command",
            Drift::ExtraCommand => "extra-command",
            Drift::MissingFlag => "missing-flag",
            Drift::ExtraFlag => "extra-flag",
            Drift::RemovedSurfacePresent => "removed-surface-present",
            Drift::GroupingOrderHelp => "grouping-order-help",
            Drift::GroupingOrderBare => "grouping-order-bare",
            Drift::BareHelpDivergence => "bare-vs-help-divergence",
        }
    }
}

/// Per-command flag set parsed from the spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlagSet {
    /// Short flags (e.g. `f` for `-f`).
    pub short: BTreeSet<String>,
    /// Long flags (e.g. `follow` for `--follow`).
    pub long: BTreeSet<String>,
}

/// Parsed spec-side surface contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecSurface {
    /// FR1 group headings in declared order, paired with the commands
    /// the spec lists under each.
    pub groups: Vec<(String, Vec<String>)>,
    /// All commands across all groups, in declared order.
    pub commands: Vec<String>,
    /// Commands the `Removed surface` table forbids.
    pub removed_commands: Vec<String>,
    /// Per-command flag set parsed from `### Logs UX` / `### Msg Modes`.
    pub flags: BTreeMap<String, FlagSet>,
}

/// Parsed binary surface (one of `loom --help` or bare `loom`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySurface {
    pub groups: Vec<(String, Vec<String>)>,
    pub commands: Vec<String>,
}

/// Spec-side parse of FR1 + flag tables + removed-surface table.
pub fn parse_spec_surface(spec_path: &Path) -> Result<SpecSurface, SurfaceError> {
    if !spec_path.is_file() {
        return Err(SurfaceError::NoSpec(spec_path.to_path_buf()));
    }
    let body = fs::read_to_string(spec_path).map_err(|source| SurfaceError::ReadFile {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let groups = parse_fr1_groups(&body)?;
    let mut commands: Vec<String> = Vec::new();
    for (_, cmds) in &groups {
        for c in cmds {
            if !commands.contains(c) {
                commands.push(c.clone());
            }
        }
    }
    let removed_commands = parse_removed_commands(&body);
    let mut flags: BTreeMap<String, FlagSet> = BTreeMap::new();
    if let Some(logs) = parse_named_flag_table(&body, "Logs UX") {
        flags.insert("logs".to_string(), logs);
    }
    if let Some(msg) = parse_named_flag_table(&body, "Msg Modes") {
        flags.insert("msg".to_string(), msg);
    }
    Ok(SpecSurface {
        groups,
        commands,
        removed_commands,
        flags,
    })
}

/// Parse FR1's three group sections (`**Workflow** —`, `**Inspection** —`,
/// `**State** —`). Each is followed by a bullet list whose first inline
/// code span on each item is `loom <name>` (or `loom <name> <arg>`).
fn parse_fr1_groups(body: &str) -> Result<Vec<(String, Vec<String>)>, SurfaceError> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for name in &["Workflow", "Inspection", "State"] {
        let cmds = parse_group_commands(body, name).ok_or_else(|| {
            SurfaceError::MalformedSpec(format!(
                "FR1 group `**{name}** —` not found or has no command bullets"
            ))
        })?;
        if cmds.is_empty() {
            return Err(SurfaceError::MalformedSpec(format!(
                "FR1 group `**{name}** —` has no command bullets"
            )));
        }
        out.push(((*name).to_string(), cmds));
    }
    Ok(out)
}

fn parse_group_commands(body: &str, group: &str) -> Option<Vec<String>> {
    let needle = format!("**{group}** —");
    let start = body.find(&needle)?;
    // Scan from `start` until the next group marker or end-of-FR1
    // (heralded by the standalone numbered list resuming at "2.").
    let tail = &body[start..];
    let next_group = ["**Workflow** —", "**Inspection** —", "**State** —"]
        .iter()
        .filter(|m| !m.starts_with(&format!("**{group}**")))
        .filter_map(|m| tail[needle.len()..].find(m).map(|i| i + needle.len()))
        .min();
    // FR1 ends at the next FR (a line beginning "2. **").
    let fr_end = tail.find("\n2. **").unwrap_or(tail.len());
    let slice_end = next_group.map(|i| i.min(fr_end)).unwrap_or(fr_end);
    let slice = &tail[..slice_end];
    let mut cmds: Vec<String> = Vec::new();
    for line in slice.lines() {
        // The list bullets are indented; the command name sits in an
        // opening inline-code span like `` `loom plan` `` or
        // `` `loom use <label>` ``.
        let trimmed = line.trim_start();
        let stripped = match trimmed.strip_prefix("- ") {
            Some(s) => s,
            None => continue,
        };
        let after_tick = match stripped.strip_prefix("`loom ") {
            Some(s) => s,
            None => continue,
        };
        let inside_end = match after_tick.find('`') {
            Some(i) => i,
            None => continue,
        };
        let token = &after_tick[..inside_end];
        // First whitespace-bounded word is the subcommand name.
        let name = token.split_whitespace().next().unwrap_or("").to_string();
        if !name.is_empty() && !cmds.contains(&name) {
            cmds.push(name);
        }
    }
    Some(cmds)
}

/// Parse the `Removed surface` markdown table inside FR1. The table is
/// identified by a `| Surface | Removed because |` header row preceded
/// by the prose phrase "Removed surface.".
fn parse_removed_commands(body: &str) -> Vec<String> {
    let anchor = match body.find("Removed surface.") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let tail = &body[anchor..];
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    for line in tail.lines() {
        let t = line.trim_start();
        if t.starts_with("| Surface ") {
            in_table = true;
            continue;
        }
        if in_table {
            if !t.starts_with('|') {
                if t.is_empty() {
                    continue;
                }
                break;
            }
            // Skip separator row.
            if t.chars()
                .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
            {
                continue;
            }
            if let Some(first_cell) = first_table_cell(t)
                && let Some(name) = extract_command_from_cell(&first_cell)
                && !out.contains(&name)
            {
                out.push(name);
            }
        }
    }
    out
}

fn first_table_cell(line: &str) -> Option<String> {
    let t = line.trim();
    let stripped = t.strip_prefix('|')?;
    let end = stripped.find('|')?;
    Some(stripped[..end].trim().to_string())
}

/// Recover `loom <name>` from a markdown cell like `` `loom doctor` `` or
/// `` `loom sync` ``. Returns the bare subcommand name.
fn extract_command_from_cell(cell: &str) -> Option<String> {
    let inside = cell.trim().strip_prefix('`')?.strip_suffix('`')?;
    let stripped = inside.strip_prefix("loom ")?;
    let name = stripped.split_whitespace().next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Parse a `### <heading>` flag table. Walks lines between the H3
/// heading and the next H2/H3 heading, then extracts `-x` / `--name`
/// tokens from any pipe-table rows it sees.
fn parse_named_flag_table(body: &str, heading: &str) -> Option<FlagSet> {
    let target = format!("### {heading}");
    let mut lines = body.lines();
    let mut found = false;
    for line in lines.by_ref() {
        if line.trim() == target {
            found = true;
            break;
        }
    }
    if !found {
        return None;
    }
    let mut set = FlagSet::default();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            break;
        }
        if !trimmed.starts_with('|') {
            continue;
        }
        for token in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
            if let Some(long) = token.strip_prefix("--") {
                if is_valid_flag_name(long) {
                    set.long.insert(long.to_string());
                }
            } else if let Some(short) = token.strip_prefix('-')
                && short.len() == 1
                && short.chars().all(|c| c.is_ascii_alphabetic())
            {
                set.short.insert(short.to_string());
            }
        }
    }
    if set.short.is_empty() && set.long.is_empty() {
        return None;
    }
    Some(set)
}

fn is_valid_flag_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && s.chars().next().map(|c| c.is_ascii_alphabetic()) == Some(true)
}

/// Trait so unit tests can supply rendered help text directly.
pub trait HelpProvider {
    fn root_help(&self) -> Result<String, SurfaceError>;
    fn bare(&self) -> Result<String, SurfaceError>;
    fn command_help(&self, cmd: &str) -> Result<String, SurfaceError>;
}

/// Real provider that shells out to a built `loom` binary.
pub struct BinaryHelp {
    pub binary: PathBuf,
}

impl BinaryHelp {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, SurfaceError> {
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .map_err(|source| SurfaceError::InvokeBinary {
                binary: self.binary.clone(),
                args: args.join(" "),
                source,
            })?;
        // Bare `loom` exits 2 because clap treats the missing
        // subcommand as a usage error; help still lands on stderr.
        // For `--help` clap exits 0 with the help on stdout.
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !stdout.is_empty() {
            return Ok(stdout);
        }
        if !stderr.is_empty() {
            return Ok(stderr);
        }
        Err(SurfaceError::BinaryFailed {
            cmd: format!("{} {}", self.binary.display(), args.join(" ")),
            code: output.status.code().unwrap_or(-1),
            stderr: String::new(),
        })
    }
}

impl HelpProvider for BinaryHelp {
    fn root_help(&self) -> Result<String, SurfaceError> {
        if !self.binary.is_file() {
            return Err(SurfaceError::NoBinary(self.binary.clone()));
        }
        self.run(&["--help"])
    }

    fn bare(&self) -> Result<String, SurfaceError> {
        if !self.binary.is_file() {
            return Err(SurfaceError::NoBinary(self.binary.clone()));
        }
        self.run(&[])
    }

    fn command_help(&self, cmd: &str) -> Result<String, SurfaceError> {
        if !self.binary.is_file() {
            return Err(SurfaceError::NoBinary(self.binary.clone()));
        }
        self.run(&[cmd, "--help"])
    }
}

/// Parse a `loom --help` / bare-`loom` rendering into a typed surface.
/// Recognises group lines `Workflow:` / `Inspection:` / `State:` and
/// collects each `  <name>  <description>` line that follows.
pub fn parse_help_surface(help: &str) -> BinarySurface {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in help.lines() {
        let trimmed = line.trim();
        if matches!(trimmed, "Workflow:" | "Inspection:" | "State:") {
            if let Some(g) = current.take() {
                groups.push(g);
            }
            current = Some((trimmed.trim_end_matches(':').to_string(), Vec::new()));
            continue;
        }
        // Any non-indented or empty line ends a group block.
        if let Some(g) = &mut current {
            if line.is_empty() || !line.starts_with(' ') {
                groups.push(g.clone());
                current = None;
                continue;
            }
            // A subcommand line is indented and starts with a bare
            // identifier. `help` (clap's built-in) is filtered out.
            let mut iter = line.split_whitespace();
            let first = iter.next().unwrap_or("");
            if first == "help" {
                continue;
            }
            if first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !first.is_empty()
            {
                g.1.push(first.to_string());
            }
        }
    }
    if let Some(g) = current {
        groups.push(g);
    }
    let mut commands: Vec<String> = Vec::new();
    for (_, cmds) in &groups {
        for c in cmds {
            if !commands.contains(c) {
                commands.push(c.clone());
            }
        }
    }
    BinarySurface { groups, commands }
}

/// Parse one subcommand's `--help` output into a flag set, filtering
/// out the auto-built / inherited flags.
pub fn parse_command_flags(help: &str) -> FlagSet {
    const INHERITED_LONG: &[&str] = &["help", "version", "workspace", "agent"];
    const INHERITED_SHORT: &[&str] = &["h", "V", "w", "A"];
    let mut set = FlagSet::default();
    let mut in_options = false;
    for line in help.lines() {
        let trimmed = line.trim_end();
        if trimmed == "Options:" {
            in_options = true;
            continue;
        }
        if !in_options {
            continue;
        }
        if line.is_empty() || !line.starts_with(' ') {
            break;
        }
        let leading = line.trim_start();
        // Continuation lines (description wrap) don't start with `-`.
        if !leading.starts_with('-') {
            continue;
        }
        // Forms emitted by clap:
        //   `-x, --long <ARG>      desc`
        //   `    --long <ARG>      desc`
        //   `-x                    desc`
        let head = leading
            .split("  ")
            .next()
            .unwrap_or("")
            .trim_end_matches(',')
            .trim();
        for token in head.split(',') {
            let token = token.trim();
            let glyph = token
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches(',');
            if let Some(long) = glyph.strip_prefix("--") {
                let name = long.trim_end_matches(',').to_string();
                if !INHERITED_LONG.contains(&name.as_str()) && is_valid_flag_name(&name) {
                    set.long.insert(name);
                }
            } else if let Some(short) = glyph.strip_prefix('-')
                && short.len() == 1
            {
                let s = short.to_string();
                if !INHERITED_SHORT.contains(&s.as_str())
                    && short.chars().all(|c| c.is_ascii_alphabetic())
                {
                    set.short.insert(s);
                }
            }
        }
    }
    set
}

/// Walk the four FR13 dimensions and return every detected drift.
pub fn audit<H: HelpProvider>(
    spec: &SpecSurface,
    help: &H,
) -> Result<Vec<SurfaceFinding>, SurfaceError> {
    let root = help.root_help()?;
    let bare = help.bare()?;
    let root_surface = parse_help_surface(&root);
    let bare_surface = parse_help_surface(&bare);

    let mut findings = Vec::new();
    diff_commands(spec, &root_surface, &mut findings);
    diff_grouping(spec, &root_surface, Drift::GroupingOrderHelp, &mut findings);
    diff_grouping(spec, &bare_surface, Drift::GroupingOrderBare, &mut findings);
    if root_surface != bare_surface {
        findings.push(SurfaceFinding {
            kind: Drift::BareHelpDivergence,
            detail: format!(
                "`loom --help` and bare `loom` differ: \
                 help-groups={help_groups:?} bare-groups={bare_groups:?}",
                help_groups = root_surface.groups,
                bare_groups = bare_surface.groups,
            ),
        });
    }
    diff_removed(spec, &root_surface, &mut findings);
    diff_flags(spec, help, &mut findings)?;
    findings.sort_by(|a, b| a.kind.tag().cmp(b.kind.tag()).then(a.detail.cmp(&b.detail)));
    Ok(findings)
}

fn diff_commands(spec: &SpecSurface, bin: &BinarySurface, out: &mut Vec<SurfaceFinding>) {
    let spec_set: BTreeSet<&str> = spec.commands.iter().map(String::as_str).collect();
    let bin_set: BTreeSet<&str> = bin.commands.iter().map(String::as_str).collect();
    for missing in spec_set.difference(&bin_set) {
        out.push(SurfaceFinding {
            kind: Drift::MissingCommand,
            detail: format!(
                "spec FR1 declares `loom {missing}` but the binary's grouped \
                 help does not list it"
            ),
        });
    }
    for extra in bin_set.difference(&spec_set) {
        out.push(SurfaceFinding {
            kind: Drift::ExtraCommand,
            detail: format!("binary advertises `loom {extra}` but no FR1 group lists it"),
        });
    }
}

fn diff_grouping(
    spec: &SpecSurface,
    bin: &BinarySurface,
    kind: Drift,
    out: &mut Vec<SurfaceFinding>,
) {
    let spec_order: Vec<&str> = spec.groups.iter().map(|(n, _)| n.as_str()).collect();
    let bin_order: Vec<&str> = bin.groups.iter().map(|(n, _)| n.as_str()).collect();
    if spec_order != bin_order {
        out.push(SurfaceFinding {
            kind,
            detail: format!(
                "expected groups {spec_order:?} (FR1 order) but binary rendered \
                 {bin_order:?}"
            ),
        });
    }
}

fn diff_removed(spec: &SpecSurface, bin: &BinarySurface, out: &mut Vec<SurfaceFinding>) {
    let bin_set: BTreeSet<&str> = bin.commands.iter().map(String::as_str).collect();
    for removed in &spec.removed_commands {
        if bin_set.contains(removed.as_str()) {
            out.push(SurfaceFinding {
                kind: Drift::RemovedSurfacePresent,
                detail: format!(
                    "spec's `Removed surface` table lists `loom {removed}` but the \
                     binary still exposes it"
                ),
            });
        }
    }
}

fn diff_flags<H: HelpProvider>(
    spec: &SpecSurface,
    help: &H,
    out: &mut Vec<SurfaceFinding>,
) -> Result<(), SurfaceError> {
    for (cmd, spec_flags) in &spec.flags {
        let cmd_help = help.command_help(cmd)?;
        let bin_flags = parse_command_flags(&cmd_help);
        for missing in spec_flags.short.difference(&bin_flags.short) {
            out.push(SurfaceFinding {
                kind: Drift::MissingFlag,
                detail: format!(
                    "spec lists `-{missing}` for `loom {cmd}` but the binary's \
                     help does not"
                ),
            });
        }
        for extra in bin_flags.short.difference(&spec_flags.short) {
            out.push(SurfaceFinding {
                kind: Drift::ExtraFlag,
                detail: format!(
                    "binary exposes `-{extra}` for `loom {cmd}` but the spec's \
                     flag table does not list it"
                ),
            });
        }
        for missing in spec_flags.long.difference(&bin_flags.long) {
            out.push(SurfaceFinding {
                kind: Drift::MissingFlag,
                detail: format!(
                    "spec lists `--{missing}` for `loom {cmd}` but the binary's \
                     help does not"
                ),
            });
        }
        for extra in bin_flags.long.difference(&spec_flags.long) {
            out.push(SurfaceFinding {
                kind: Drift::ExtraFlag,
                detail: format!(
                    "binary exposes `--{extra}` for `loom {cmd}` but the spec's \
                     flag table does not list it"
                ),
            });
        }
    }
    Ok(())
}

/// Print findings; return the process exit code. `0` ↔ clean, `1` ↔
/// any drift.
pub fn report(findings: &[SurfaceFinding]) -> i32 {
    for f in findings {
        eprintln!(
            "{tag} {detail}",
            tag = f.kind.tag().to_uppercase(),
            detail = f.detail,
        );
    }
    eprintln!(
        "loom check surface: {n} drift{plural}",
        n = findings.len(),
        plural = if findings.len() == 1 { "" } else { "s" },
    );
    if findings.is_empty() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_path() -> PathBuf {
        PathBuf::from("/non/existent")
    }

    #[test]
    fn parse_fr1_groups_extracts_three_groups_in_order() {
        let body = "\
1. **Command set** — commands fall into three groups.

   **Workflow** — the loom loop:
   - `loom plan` — interview
   - `loom todo` — decomposition

   **Inspection** — read-only:
   - `loom status` — print state

   **State** — workspace lifecycle:
   - `loom init` — bootstrap

2. **Compiled templates** — Askama.
";
        let groups = parse_fr1_groups(body).expect("parse");
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].0, "Workflow");
        assert_eq!(groups[0].1, vec!["plan", "todo"]);
        assert_eq!(groups[1].0, "Inspection");
        assert_eq!(groups[1].1, vec!["status"]);
        assert_eq!(groups[2].0, "State");
        assert_eq!(groups[2].1, vec!["init"]);
    }

    #[test]
    fn parse_fr1_groups_handles_multi_word_command_bullets() {
        let body = "\
   **Workflow** — order:
   - `loom plan` — interview
   - `loom run --once` — single bead

   **Inspection** — read-only:
   - `loom status` — print

   **State** — state:
   - `loom use <label>` — set spec
2. **Compiled templates** — Askama.
";
        let groups = parse_fr1_groups(body).expect("parse");
        assert_eq!(groups[0].1, vec!["plan", "run"]);
        assert_eq!(groups[2].1, vec!["use"]);
    }

    #[test]
    fn parse_removed_commands_extracts_table_rows() {
        let body = "\
Removed surface. The table lists removed commands.

   | Surface | Removed because |
   |---------|-----------------|
   | `loom doctor` | replaced by audits |
   | `loom sync` | unneeded |
   | `loom tune` | unneeded |

Next paragraph.
";
        let removed = parse_removed_commands(body);
        assert_eq!(removed, vec!["doctor", "sync", "tune"]);
    }

    #[test]
    fn parse_removed_commands_empty_when_table_missing() {
        let body = "No removed-surface section here.\n";
        assert!(parse_removed_commands(body).is_empty());
    }

    #[test]
    fn parse_help_surface_extracts_groups_and_commands_in_order() {
        let help = "\
Loom harness CLI

Usage: loom [OPTIONS] <COMMAND>

Workflow:
  plan    Interview
  todo    Decompose
  run     Per-bead loop

Inspection:
  status  Print state

State:
  init    Bootstrap
  use     Set spec

  help    Print help

Options:
  -h, --help  Print help
";
        let surface = parse_help_surface(help);
        assert_eq!(
            surface
                .groups
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["Workflow", "Inspection", "State"],
        );
        let workflow = &surface.groups[0].1;
        assert_eq!(workflow, &vec!["plan", "todo", "run"]);
        let state = &surface.groups[2].1;
        assert_eq!(state, &vec!["init", "use"]);
        // `help` (clap built-in) and the Options block must not leak
        // into the command set.
        assert!(!surface.commands.contains(&"help".to_string()));
        assert!(!surface.commands.contains(&"-h".to_string()));
    }

    #[test]
    fn parse_command_flags_filters_inherited_and_built_in() {
        let help = "\
Render logs.

Usage: loom logs [OPTIONS]

Options:
  -b, --bead <ID>      Restrict to bead
  -w, --workspace <PATH>  Workspace
  -A, --agent <BACKEND>   Agent backend
  -f, --follow         Tail
      --raw            Raw bytes
  -v, --verbose        Verbose
      --path           Print path
  -h, --help           Print help
";
        let flags = parse_command_flags(help);
        assert!(flags.short.contains("b"));
        assert!(flags.short.contains("f"));
        assert!(flags.short.contains("v"));
        assert!(flags.long.contains("bead"));
        assert!(flags.long.contains("follow"));
        assert!(flags.long.contains("raw"));
        assert!(flags.long.contains("verbose"));
        assert!(flags.long.contains("path"));
        // Inherited / auto-built must be filtered.
        assert!(!flags.short.contains("h"));
        assert!(!flags.short.contains("w"));
        assert!(!flags.short.contains("A"));
        assert!(!flags.long.contains("help"));
        assert!(!flags.long.contains("workspace"));
        assert!(!flags.long.contains("agent"));
    }

    #[test]
    fn parse_named_flag_table_extracts_flags_from_logs_ux() {
        let body = "\
### Logs UX

| Flag | Behavior |
|------|----------|
| (default) | render |
| `-f` / `--follow` | tail |
| `-b` / `--bead <id>` | select |
| `--raw` | raw bytes |

### Next Section
";
        let set = parse_named_flag_table(body, "Logs UX").expect("parsed");
        assert!(set.short.contains("f"));
        assert!(set.short.contains("b"));
        assert!(set.long.contains("follow"));
        assert!(set.long.contains("bead"));
        assert!(set.long.contains("raw"));
    }

    #[test]
    fn parse_named_flag_table_returns_none_when_section_absent() {
        let body = "## Other section\n\nbody\n";
        assert!(parse_named_flag_table(body, "Logs UX").is_none());
    }

    // -- audit-level tests using a stubbed help provider --

    struct StubHelp {
        root: String,
        bare: String,
        commands: BTreeMap<String, String>,
    }

    impl HelpProvider for StubHelp {
        fn root_help(&self) -> Result<String, SurfaceError> {
            Ok(self.root.clone())
        }
        fn bare(&self) -> Result<String, SurfaceError> {
            Ok(self.bare.clone())
        }
        fn command_help(&self, cmd: &str) -> Result<String, SurfaceError> {
            self.commands
                .get(cmd)
                .cloned()
                .ok_or_else(|| SurfaceError::MalformedSpec(format!("no stub help for `{cmd}`")))
        }
    }

    fn ok_help() -> String {
        "Usage: loom [OPTIONS] <COMMAND>

Workflow:
  plan    Interview
  todo    Decompose

Inspection:
  status  Print state

State:
  init    Bootstrap

Options:
  -h, --help  Print help
"
        .to_string()
    }

    fn ok_spec() -> SpecSurface {
        SpecSurface {
            groups: vec![
                ("Workflow".into(), vec!["plan".into(), "todo".into()]),
                ("Inspection".into(), vec!["status".into()]),
                ("State".into(), vec!["init".into()]),
            ],
            commands: vec!["plan".into(), "todo".into(), "status".into(), "init".into()],
            removed_commands: vec!["doctor".into()],
            flags: BTreeMap::new(),
        }
    }

    #[test]
    fn audit_clean_when_spec_matches_binary() {
        let help = ok_help();
        let stub = StubHelp {
            root: help.clone(),
            bare: help,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(f.is_empty(), "expected clean audit, got: {f:?}");
    }

    #[test]
    fn audit_flags_missing_command() {
        let help = "Workflow:
  plan    Interview

Inspection:
  status  Print

State:
  init    Bootstrap
"
        .to_string();
        let stub = StubHelp {
            root: help.clone(),
            bare: help,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter()
                .any(|x| x.kind == Drift::MissingCommand && x.detail.contains("todo")),
            "expected missing-command for `todo`, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_extra_command() {
        let help = "Workflow:
  plan    Interview
  todo    Decompose
  surprise Stowaway

Inspection:
  status  Print

State:
  init    Bootstrap
"
        .to_string();
        let stub = StubHelp {
            root: help.clone(),
            bare: help,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter()
                .any(|x| x.kind == Drift::ExtraCommand && x.detail.contains("surprise")),
            "expected extra-command for `surprise`, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_grouping_order_in_help() {
        // Groups out of order in --help.
        let help = "State:
  init    Bootstrap

Workflow:
  plan    Interview
  todo    Decompose

Inspection:
  status  Print
"
        .to_string();
        let stub = StubHelp {
            root: help.clone(),
            bare: help,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter().any(|x| x.kind == Drift::GroupingOrderHelp),
            "expected grouping-order-help, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_grouping_order_in_bare() {
        let good = ok_help();
        let bad = "State:
  init    Bootstrap

Inspection:
  status  Print

Workflow:
  plan    Interview
  todo    Decompose
"
        .to_string();
        let stub = StubHelp {
            root: good,
            bare: bad,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter().any(|x| x.kind == Drift::GroupingOrderBare),
            "expected grouping-order-bare, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_removed_surface_resurfacing() {
        let help = "Workflow:
  plan    Interview
  todo    Decompose
  doctor  Resurfaced

Inspection:
  status  Print

State:
  init    Bootstrap
"
        .to_string();
        // We expect both ExtraCommand and RemovedSurfacePresent.
        let stub = StubHelp {
            root: help.clone(),
            bare: help,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter()
                .any(|x| x.kind == Drift::RemovedSurfacePresent && x.detail.contains("doctor")),
            "expected removed-surface-present for `doctor`, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_bare_vs_help_divergence() {
        let help = ok_help();
        let bare = "Workflow:
  plan    Interview
  todo    Decompose
  status  Print  <-- misplaced

Inspection:

State:
  init    Bootstrap
"
        .to_string();
        let stub = StubHelp {
            root: help,
            bare,
            commands: BTreeMap::new(),
        };
        let f = audit(&ok_spec(), &stub).expect("audit");
        assert!(
            f.iter().any(|x| x.kind == Drift::BareHelpDivergence),
            "expected bare-vs-help-divergence, got: {f:?}",
        );
    }

    #[test]
    fn audit_flags_missing_and_extra_flags() {
        let help = ok_help();
        let logs_help = "\
Usage: loom logs [OPTIONS]

Options:
  -b, --bead <ID>     Restrict
  -f, --follow        Tail
  -x, --extra         Stowaway flag
  -h, --help          Print help
";
        let mut spec = ok_spec();
        spec.flags.insert(
            "logs".into(),
            FlagSet {
                short: BTreeSet::from(["b".into(), "f".into(), "v".into()]),
                long: BTreeSet::from(["bead".into(), "follow".into(), "verbose".into()]),
            },
        );
        spec.commands.push("logs".into());
        spec.groups[1].1.push("logs".into());
        let mut root = help;
        root.insert_str(
            root.find("Inspection:\n  status  Print state\n")
                .map(|i| i + "Inspection:\n  status  Print state\n".len())
                .unwrap_or(root.len()),
            "  logs    Render\n",
        );
        let stub = StubHelp {
            root: root.clone(),
            bare: root,
            commands: BTreeMap::from([("logs".into(), logs_help.into())]),
        };
        let f = audit(&spec, &stub).expect("audit");
        assert!(
            f.iter()
                .any(|x| x.kind == Drift::MissingFlag && x.detail.contains("--verbose")),
            "expected missing-flag --verbose: {f:?}",
        );
        assert!(
            f.iter()
                .any(|x| x.kind == Drift::ExtraFlag && x.detail.contains("--extra")),
            "expected extra-flag --extra: {f:?}",
        );
    }

    #[test]
    fn report_exit_zero_on_clean() {
        assert_eq!(report(&[]), 0);
    }

    #[test]
    fn report_exit_one_on_any_drift() {
        let f = vec![SurfaceFinding {
            kind: Drift::MissingCommand,
            detail: "x".into(),
        }];
        assert_eq!(report(&f), 1);
    }

    #[test]
    fn binary_help_reports_missing_binary() {
        let h = BinaryHelp::new(dummy_path());
        let err = h.root_help().expect_err("must fail");
        assert!(matches!(err, SurfaceError::NoBinary(_)));
    }
}
