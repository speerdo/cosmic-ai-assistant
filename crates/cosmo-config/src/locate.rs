//! Finding the MCP agent's executable when PATH doesn't have it (phase-8
//! §8.1).
//!
//! `computer-use-linux` is an npm package, usually installed under nvm or a
//! user npm prefix. A systemd user unit's PATH includes neither, and its
//! `#!/usr/bin/env node` shebang needs `node`, which nvm keeps in the same
//! directory. So the daemon looks where npm puts things, and runs the agent
//! with that directory first on PATH.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where an agent was found, and the PATH to run it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub program: PathBuf,
    /// The agent's own directory, then the inherited PATH.
    pub path_env: OsString,
}

/// Find `command`: as given when it names a path, else on `PATH`, else in
/// the user's npm locations. `None` when it's nowhere.
pub fn agent(command: &str) -> Option<Located> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    locate(command, env("PATH"), env("HOME"), env("NPM_CONFIG_PREFIX"))
}

/// [`agent`], with the environment passed in (for tests).
pub fn locate(
    command: &str,
    path: Option<OsString>,
    home: Option<OsString>,
    npm_prefix: Option<OsString>,
) -> Option<Located> {
    let path_dirs: Vec<PathBuf> = path
        .as_ref()
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let program = if command.contains('/') {
        let p = PathBuf::from(command);
        is_executable(&p).then_some(p)?
    } else {
        path_dirs
            .iter()
            .chain(fallback_dirs(home.as_deref().map(Path::new), npm_prefix).iter())
            .map(|d| d.join(command))
            .find(|p| is_executable(p))?
    };
    let own = program.parent().map(Path::to_path_buf);
    let path_env = std::env::join_paths(
        own.into_iter()
            .chain(path_dirs.into_iter())
            .chain(["/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from)),
    )
    .ok()?;
    Some(Located { program, path_env })
}

/// npm's usual homes for global packages, newest nvm node first.
fn fallback_dirs(home: Option<&Path>, npm_prefix: Option<OsString>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(prefix) = npm_prefix {
        dirs.push(PathBuf::from(prefix).join("bin"));
    }
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
        let mut nvm: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(home.join(".nvm/versions/node"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| (version_key(&e.file_name().to_string_lossy()), e.path()))
            .collect();
        nvm.sort_by(|a, b| b.0.cmp(&a.0));
        dirs.extend(nvm.into_iter().map(|(_, p)| p.join("bin")));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// "v22.11.0" → [22, 11, 0], for newest-first ordering.
fn version_key(name: &str) -> Vec<u64> {
    name.trim_start_matches('v')
        .split('.')
        .map(|n| n.parse().unwrap_or(0))
        .collect()
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cosmo-locate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn path_wins_over_fallbacks() {
        let root = tmp("path");
        exe(&root.join("onpath/agent"));
        exe(&root.join("home/.local/bin/agent"));
        let got = locate(
            "agent",
            Some(root.join("onpath").into()),
            Some(root.join("home").into()),
            None,
        )
        .unwrap();
        assert_eq!(got.program, root.join("onpath/agent"));
    }

    #[test]
    fn newest_nvm_node_is_found_and_put_first_on_path() {
        let root = tmp("nvm");
        let home = root.join("home");
        exe(&home.join(".nvm/versions/node/v9.0.0/bin/agent"));
        exe(&home.join(".nvm/versions/node/v22.11.0/bin/agent"));
        let got = locate(
            "agent",
            Some("/usr/bin".into()),
            Some(home.clone().into()),
            None,
        )
        .unwrap();
        let bin = home.join(".nvm/versions/node/v22.11.0/bin");
        assert_eq!(got.program, bin.join("agent"));
        let first = std::env::split_paths(&got.path_env).next().unwrap();
        assert_eq!(first, bin, "node lives beside the agent under nvm");
    }

    #[test]
    fn not_executable_or_missing_is_none() {
        let root = tmp("none");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/agent"), "").unwrap();
        assert_eq!(
            locate("agent", Some(root.join("bin").into()), None, None),
            None
        );
        assert_eq!(locate("/nonexistent/agent", None, None, None), None);
    }
}
