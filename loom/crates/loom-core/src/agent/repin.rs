use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Re-pin payload that restores an agent's working memory after compaction.
///
/// Both backends consume the same content; only delivery differs (see
/// [`Self::to_prompt`] vs [`Self::write_claude_files`]). The workflow engine
/// builds one of these per session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RePinContent {
    /// Short orientation banner (label, command, mode) — first thing the
    /// agent reads after compaction.
    pub orientation: String,

    /// Stable context block — spec path, exit signals, companions index.
    pub pinned_context: String,

    /// Additional partial bodies (template snippets, role-specific guidance)
    /// appended verbatim. May be empty.
    pub partial_bodies: Vec<String>,
}

impl RePinContent {
    /// Render the re-pin payload as a single prompt string for delivery via
    /// pi's `steer` command.
    ///
    /// The format is stable: orientation, blank line, pinned context, then
    /// each partial body separated by a blank line. No backend-specific
    /// framing — the caller wraps this in the appropriate command envelope.
    ///
    /// ```
    /// use loom_core::agent::RePinContent;
    ///
    /// let r = RePinContent {
    ///     orientation: "loom run @ wx-1".to_string(),
    ///     pinned_context: "Spec: specs/loom-harness.md".to_string(),
    ///     partial_bodies: vec!["partial alpha".to_string(), "partial beta".to_string()],
    /// };
    /// let prompt = r.to_prompt();
    /// assert!(prompt.starts_with("loom run @ wx-1"));
    /// assert!(prompt.contains("Spec: specs/loom-harness.md"));
    /// assert!(prompt.contains("partial alpha"));
    /// assert!(prompt.contains("partial beta"));
    /// ```
    pub fn to_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.orientation);
        out.push_str("\n\n");
        out.push_str(&self.pinned_context);
        for body in &self.partial_bodies {
            out.push_str("\n\n");
            out.push_str(body);
        }
        out
    }

    /// Write the two files that claude's `SessionStart` hook reads on each
    /// compaction event:
    ///
    /// - `repin.sh` — executable script that prints the
    ///   `hookSpecificOutput` JSON envelope on stdout. The orientation +
    ///   pinned context are baked in via a heredoc, so the script needs no
    ///   external dependencies (no `jq`, no second content file).
    /// - `claude-settings.json` — settings fragment installing the hook
    ///   under `SessionStart[matcher: "compact"]` pointing at `repin.sh`.
    ///
    /// `runtime_dir` is created if missing. On Unix the script is chmod
    /// 0o755 so claude can exec it directly.
    ///
    /// ```
    /// use loom_core::agent::RePinContent;
    /// # fn main() -> std::io::Result<()> {
    /// let dir = std::env::temp_dir().join("loom-doctest-repin-files");
    /// // Doc test runs may be repeated — clean up first.
    /// let _ = std::fs::remove_dir_all(&dir);
    /// let r = RePinContent {
    ///     orientation: "loom check @ wx-2".to_string(),
    ///     pinned_context: "Spec: specs/loom-harness.md".to_string(),
    ///     partial_bodies: vec![],
    /// };
    /// r.write_claude_files(&dir)?;
    /// let script = std::fs::read_to_string(dir.join("repin.sh"))?;
    /// assert!(script.starts_with("#!/usr/bin/env bash"));
    /// assert!(script.contains("loom check @ wx-2"));
    /// let settings = std::fs::read_to_string(dir.join("claude-settings.json"))?;
    /// assert!(settings.contains("SessionStart"));
    /// assert!(settings.contains("compact"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn write_claude_files(&self, runtime_dir: &Path) -> Result<(), io::Error> {
        std::fs::create_dir_all(runtime_dir)?;

        let payload = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": self.to_prompt(),
            }
        });
        let payload_str = serde_json::to_string(&payload).map_err(io::Error::other)?;

        let script_path = runtime_dir.join("repin.sh");
        let mut script = String::new();
        script.push_str("#!/usr/bin/env bash\n");
        script.push_str("set -euo pipefail\n");
        script.push_str("cat <<'LOOM_REPIN_JSON_EOF'\n");
        script.push_str(&payload_str);
        script.push('\n');
        script.push_str("LOOM_REPIN_JSON_EOF\n");
        std::fs::write(&script_path, script)?;
        set_executable(&script_path)?;

        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "compact",
                        "hooks": [
                            {
                                "type": "command",
                                "command": script_path.to_string_lossy(),
                            }
                        ]
                    }
                ]
            }
        });
        let settings_str = serde_json::to_string_pretty(&settings).map_err(io::Error::other)?;
        std::fs::write(runtime_dir.join("claude-settings.json"), settings_str)?;

        Ok(())
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn to_prompt_orders_orientation_context_partials() {
        let r = RePinContent {
            orientation: "ORI".to_string(),
            pinned_context: "CTX".to_string(),
            partial_bodies: vec!["P1".to_string(), "P2".to_string()],
        };
        let s = r.to_prompt();
        assert_eq!(s, "ORI\n\nCTX\n\nP1\n\nP2");
    }

    #[test]
    fn to_prompt_omits_partials_when_empty() {
        let r = RePinContent {
            orientation: "ORI".to_string(),
            pinned_context: "CTX".to_string(),
            partial_bodies: vec![],
        };
        assert_eq!(r.to_prompt(), "ORI\n\nCTX");
    }

    #[test]
    fn write_claude_files_emits_executable_script_with_baked_payload() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let r = RePinContent {
            orientation: "loom run @ wx-3hhwq.8".to_string(),
            pinned_context: "Spec: specs/loom-harness.md".to_string(),
            partial_bodies: vec!["exit signals: RALPH_COMPLETE".to_string()],
        };
        r.write_claude_files(dir.path())?;

        let script = std::fs::read_to_string(dir.path().join("repin.sh"))?;
        assert!(script.starts_with("#!/usr/bin/env bash\n"));
        assert!(script.contains("LOOM_REPIN_JSON_EOF"));
        assert!(script.contains("hookSpecificOutput"));
        assert!(script.contains("loom run @ wx-3hhwq.8"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("repin.sh"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }

        let settings = std::fs::read_to_string(dir.path().join("claude-settings.json"))?;
        let parsed: serde_json::Value = serde_json::from_str(&settings)?;
        let hook = &parsed["hooks"]["SessionStart"][0];
        assert_eq!(hook["matcher"], "compact");
        assert_eq!(hook["hooks"][0]["type"], "command");
        let cmd = hook["hooks"][0]["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing command field"))?;
        assert!(cmd.ends_with("repin.sh"));

        Ok(())
    }
}
