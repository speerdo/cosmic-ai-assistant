//! `system_query`: read-only system facts via a fixed allowlist — `df`,
//! `ip`, `free`, `systemctl`, `sensors`, `uptime`. **No shell** (invariant
//! #2): argv vectors only, nothing user-controlled is interpolated.

use tokio::process::Command;

use crate::{ToolError, ToolOutput};

/// `(name, argv…)` the query tool may run. `name` is what the model picks;
/// the argv is fixed. Adding a query is a one-line const entry.
const ALLOWED: &[(&str, &[&str])] = &[
    ("disk", &["df", "-h"]),
    ("memory", &["free", "-h"]),
    ("network", &["ip", "-brief", "addr"]),
    (
        "failed_services",
        &["systemctl", "list-units", "--failed", "--no-pager"],
    ),
    (
        "failed_user_services",
        &[
            "systemctl",
            "--user",
            "list-units",
            "--failed",
            "--no-pager",
        ],
    ),
    ("sensors", &["sensors"]),
    ("uptime", &["uptime"]),
];

/// All query names, for the tool description / doctor.
pub fn names() -> Vec<&'static str> {
    ALLOWED.iter().map(|(n, _)| *n).collect()
}

/// Run one allowlisted query by name.
pub async fn query(name: &str) -> ToolOutput {
    let Some((_, argv)) = ALLOWED.iter().find(|(n, _)| *n == name) else {
        return Err(ToolError::Failed(
            "system_query".into(),
            format!(
                "`{name}` is not in the read-only allowlist ({}); there is no shell",
                names().join(", ")
            ),
        ));
    };
    let (bin, rest) = (argv[0], &argv[1..]);
    let output = Command::new(bin)
        .args(rest)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| ToolError::Failed("system_query".into(), format!("{bin}: {e}")))?;
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Ok(if out.trim().is_empty() { err } else { out })
    } else {
        Err(ToolError::Failed(
            "system_query".into(),
            if err.trim().is_empty() {
                format!("exit status {}", output.status)
            } else {
                err
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allowlisted_queries_run() {
        let out = query("disk").await.expect("df must run");
        assert!(out.contains("Filesystem") || out.contains("文件系统"));
    }

    #[tokio::test]
    async fn non_allowlisted_names_refused() {
        let err = query("cat /etc/passwd").await.expect_err("must refuse");
        assert!(err.to_string().contains("read-only allowlist"));
        let err = query("rm -rf /").await.expect_err("must refuse");
        assert!(err.to_string().contains("read-only allowlist"));
    }
}
