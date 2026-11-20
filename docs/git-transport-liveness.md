# Git transport liveness

Initialized repository helpers and sandbox Git defaults send authenticated SSH
keepalives every 30 seconds, with 20 unanswered probes allowed. This keeps an
otherwise idle receive-pack connection alive while ordinary local hooks run. It
does not skip hooks, reuse approvals, change verification deadlines or alter
GitHub host pins, deploy-key selection, ambient-identity exclusion or signing.

`wrix init repo` generates the options in the common-directory helper; bare
`wrix init` checks the effective OpenSSH configuration and diagnoses stale
helpers rather than modifying them. Reinitialization preserves the stable
common-directory trampoline. An independent Loom integration clone still needs
its own initialization; initializing an outer checkout does not configure it.

Native acceptance executes generated helpers with both supported key-resolution
paths and compares complete SSH argv. Reinitialization repairs a stale helper
without changing common Git configuration. Sandbox acceptance executes the
bootstrap twice, checks real `ssh -G` settings and retains repository config.
Actual publication remains separate from these argument/configuration controls.
