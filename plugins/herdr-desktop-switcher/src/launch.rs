use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::parse::{Desktop, DesktopConfig, DesktopMode, KeybindingAuthority, load_path};
use crate::registry::{InstanceRecord, Registry};

const BUNDLE_ID: &str = "com.gustavocaiano.herdr";
const HELPER_TIMEOUT: Duration = Duration::from_secs(25);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const SSH_CONFIG_TIMEOUT: Duration = Duration::from_secs(3);
const SSH_TIMEOUT: Duration = Duration::from_secs(8);
const NESTED_ENVIRONMENT: &[&str] = &[
    "HERDR_ENV",
    "HERDR_SOCKET_PATH",
    "HERDR_SESSION",
    "HERDR_WORKSPACE_ID",
    "HERDR_PANE_ID",
    "HERDR_ACTIVE_WORKSPACE_ID",
    "HERDR_ACTIVE_TAB_ID",
    "HERDR_ACTIVE_PANE_ID",
    "HERDR_ACTIVE_PANE_CWD",
];

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub home: PathBuf,
    pub app: PathBuf,
    pub command_shim: PathBuf,
    pub config: PathBuf,
    pub launcher: PathBuf,
    pub real_herdr: PathBuf,
    pub ssh: PathBuf,
    pub state_dir: PathBuf,
    pub switcher_bin: PathBuf,
}

impl RuntimePaths {
    pub fn from_env() -> Result<Self, LaunchError> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| LaunchError("HOME is not set".into()))?;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = env_path("HERDR_DESKTOP_REPO").unwrap_or_else(|| {
            manifest
                .parent()
                .and_then(Path::parent)
                .expect("crate lives under <repo>/plugins/herdr-desktop-switcher")
                .to_path_buf()
        });
        Ok(Self {
            home: home.clone(),
            app: env_path("HERDR_APP_PATH").unwrap_or_else(|| home.join("Applications/Herdr.app")),
            command_shim: env_path("HERDR_DESKTOP_COMMAND")
                .unwrap_or_else(|| repo.join("scripts/herdr-desktop-command")),
            config: env_path("HERDR_DESKTOPS_TOML").unwrap_or_else(|| repo.join("desktops.toml")),
            launcher: env_path("HERDR_DESKTOP_LAUNCHER")
                .unwrap_or_else(|| home.join(".local/bin/herdr-desktop-launch")),
            real_herdr: env_path("HERDR_REAL_BIN").unwrap_or_else(|| home.join(".local/bin/herdr")),
            ssh: env_path("HERDR_SSH_BIN").unwrap_or_else(|| PathBuf::from("/usr/bin/ssh")),
            state_dir: env_path("HERDR_DESKTOP_STATE_DIR")
                .unwrap_or_else(|| home.join(".local/state/herdr-desktop-switcher")),
            switcher_bin: env::current_exe().map_err(|error| {
                LaunchError(format!("could not determine switcher executable: {error}"))
            })?,
        })
    }

    pub fn validate_for_launch(&self) -> Result<(), LaunchError> {
        require_absolute(&self.home, "home directory")?;
        require_file(&self.config, "desktop configuration")?;
        require_executable(&self.command_shim, "desktop command shim")?;
        require_executable(&self.launcher, "desktop launch helper")?;
        require_executable(&self.real_herdr, "Herdr binary")?;
        require_executable(&self.switcher_bin, "desktop switcher binary")?;
        require_absolute(&self.state_dir, "state directory")
    }

    pub fn validate_for_client(&self) -> Result<(), LaunchError> {
        require_file(&self.config, "desktop configuration")?;
        require_executable(&self.real_herdr, "Herdr binary")
    }
}

pub fn expected_bundle_id(desktop: &Desktop) -> String {
    match &desktop.mode {
        DesktopMode::Local => BUNDLE_ID.to_string(),
        DesktopMode::Remote { .. } => format!("{BUNDLE_ID}.{}", desktop.id.replace('_', "-")),
    }
}

pub fn remote_app_path(home: &Path, app_name: &str) -> PathBuf {
    home.join("Applications").join(format!("{app_name}.app"))
}

fn desktop_app_path(paths: &RuntimePaths, desktop: &Desktop) -> PathBuf {
    match &desktop.mode {
        DesktopMode::Local => paths.app.clone(),
        DesktopMode::Remote { app_name, .. } => remote_app_path(&paths.home, app_name),
    }
}

fn require_desktop_app(id: &str, mode: &DesktopMode, app: &Path) -> Result<(), LaunchError> {
    match mode {
        DesktopMode::Local => require_directory(app, "Herdr app"),
        DesktopMode::Remote { .. } => require_directory(app, "desktop app").map_err(|_| {
            LaunchError(format!(
                "desktop {id:?} app is missing at {}; create it with ~/.config/herd/scripts/sync-desktop-apps.sh",
                app.display()
            ))
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum LaunchPlan {
    Local,
    Remote {
        target: String,
        session: String,
        keybindings: KeybindingAuthority,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Launched { pid: i32 },
    Focused { pid: i32 },
}

#[derive(Debug)]
pub struct LaunchError(String);

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for LaunchError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchIdentity {
    pid: i32,
    launch_date_unix_ms: i64,
    bundle_id: String,
    bundle_path: String,
}

#[derive(Debug)]
enum CommandFailure {
    Start(String),
    Wait(String),
    Timeout,
    Exit { code: Option<i32>, stderr: String },
}

impl CommandFailure {
    fn code(&self) -> Option<i32> {
        match self {
            Self::Exit { code, .. } => *code,
            _ => None,
        }
    }

    fn unknown_launch_state(&self) -> bool {
        match self {
            Self::Start(_) => false,
            Self::Wait(_) | Self::Timeout => true,
            Self::Exit { code, .. } => code.is_none() || *code == Some(75),
        }
    }
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(message) | Self::Wait(message) => formatter.write_str(message),
            Self::Timeout => formatter.write_str("command timed out"),
            Self::Exit { code, stderr } => write!(
                formatter,
                "command exited with {}{}",
                code.map_or_else(|| "signal".into(), |value| value.to_string()),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            ),
        }
    }
}

pub fn plan_desktop(config: &DesktopConfig, id: &str) -> Result<LaunchPlan, LaunchError> {
    let desktop = config
        .desktop(id)
        .ok_or_else(|| LaunchError(format!("desktop {id:?} is not defined")))?;
    Ok(match &desktop.mode {
        DesktopMode::Local => LaunchPlan::Local,
        DesktopMode::Remote {
            target,
            session,
            keybindings,
            ..
        } => LaunchPlan::Remote {
            target: target.clone(),
            session: session.clone(),
            keybindings: *keybindings,
        },
    })
}

pub fn launch_desktop(paths: &RuntimePaths, id: &str) -> Result<LaunchOutcome, LaunchError> {
    paths.validate_for_launch()?;
    let registry = Registry::new(&paths.state_dir);
    let _lock = registry
        .lock(id, LOCK_TIMEOUT)
        .map_err(|error| LaunchError(error.to_string()))?;
    let config = load_path(&paths.config).map_err(|error| LaunchError(error.to_string()))?;
    let desktop = config
        .desktop(id)
        .ok_or_else(|| LaunchError(format!("desktop {id:?} is not defined")))?;
    let app = desktop_app_path(paths, desktop);
    let bundle_id = expected_bundle_id(desktop);
    require_desktop_app(id, &desktop.mode, &app)?;
    let canonical_app = fs::canonicalize(&app).map_err(|error| {
        LaunchError(format!(
            "could not canonicalize desktop {id:?} app {}: {error}",
            app.display()
        ))
    })?;
    let plan = plan_desktop(&config, id)?;
    let expected_plan = serde_json::to_string(&plan)
        .map_err(|error| LaunchError(format!("could not encode expected launch plan: {error}")))?;
    let was_pending = registry
        .pending(id)
        .map_err(|error| LaunchError(error.to_string()))?;

    if let Some(record) = registry
        .live_for(id)
        .map_err(|error| LaunchError(error.to_string()))?
    {
        if !record_matches_desktop(&record, desktop, &canonical_app, &bundle_id) {
            return Err(LaunchError(format!(
                "desktop {id:?} configuration changed while pid {} is still open; close the old client before launching the new endpoint",
                record.pid
            )));
        }
        match invoke_identity_helper(paths, "activate", &record) {
            Ok(_) => {
                if was_pending {
                    registry
                        .clear_pending(id)
                        .map_err(|clear| LaunchError(clear.to_string()))?;
                }
                return Ok(LaunchOutcome::Focused { pid: record.pid });
            }
            Err(error) if error.code() == Some(65) => {
                registry
                    .remove(id)
                    .map_err(|remove| LaunchError(remove.to_string()))?;
                if was_pending {
                    registry
                        .clear_pending(id)
                        .map_err(|clear| LaunchError(clear.to_string()))?;
                }
            }
            Err(error) => {
                if was_pending {
                    registry
                        .clear_pending(id)
                        .map_err(|clear| LaunchError(clear.to_string()))?;
                }
                return Err(LaunchError(format!(
                    "could not focus existing desktop {id:?}: {error}"
                )));
            }
        }
    }

    if was_pending {
        return Err(LaunchError(format!(
            "desktop {id:?} has an unresolved previous launch without a verifiable registry record; inspect {} before retrying",
            paths.state_dir.join(format!("{id}.pending")).display()
        )));
    }

    if let DesktopMode::Remote { target, .. } = &desktop.mode {
        preflight_remote(paths, target)?;
    }
    registry
        .mark_pending(id)
        .map_err(|error| LaunchError(error.to_string()))?;

    let output = match run_command(
        &paths.launcher,
        &launch_arguments(paths, id, &expected_plan, &app, &bundle_id),
        HELPER_TIMEOUT,
    ) {
        Ok(output) => output,
        Err(error) => {
            if !error.unknown_launch_state() {
                registry
                    .clear_pending(id)
                    .map_err(|clear| LaunchError(clear.to_string()))?;
            }
            return Err(LaunchError(format!(
                "desktop {id:?} launch failed{}: {error}",
                if error.unknown_launch_state() {
                    "; outcome is unknown and retries are blocked"
                } else {
                    ""
                }
            )));
        }
    };

    let identity = match parse_launch_identity(&canonical_app, &bundle_id, &output.stdout) {
        Ok(identity) => identity,
        Err(error) => {
            return Err(LaunchError(format!(
                "desktop {id:?} may be running but returned untrusted identity; retries remain blocked: {error}"
            )));
        }
    };
    let record = record_from_desktop(desktop, identity);
    if let Err(write_error) = registry.write(&record) {
        match invoke_identity_helper(paths, "terminate", &record) {
            Ok(_) => {
                registry
                    .clear_pending(id)
                    .map_err(|error| LaunchError(error.to_string()))?;
                return Err(LaunchError(format!(
                    "could not record desktop {id:?}; the newly launched client was terminated: {write_error}"
                )));
            }
            Err(cleanup_error) => {
                return Err(LaunchError(format!(
                    "could not record desktop {id:?} and could not verify cleanup; retries remain blocked: {write_error}; cleanup: {cleanup_error}"
                )));
            }
        }
    }
    registry
        .clear_pending(id)
        .map_err(|error| LaunchError(error.to_string()))?;
    Ok(LaunchOutcome::Launched { pid: record.pid })
}

pub fn client_desktop(paths: &RuntimePaths, id: &str) -> Result<(), LaunchError> {
    paths.validate_for_client()?;
    let config = load_path(&paths.config).map_err(|error| LaunchError(error.to_string()))?;
    let plan = plan_desktop(&config, id)?;
    let expected_source = env::var("HERDR_DESKTOP_EXPECTED_PLAN")
        .map_err(|_| LaunchError("HERDR_DESKTOP_EXPECTED_PLAN is required".into()))?;
    let expected: LaunchPlan = serde_json::from_str(&expected_source)
        .map_err(|error| LaunchError(format!("invalid expected launch plan: {error}")))?;
    if expected != plan {
        return Err(LaunchError(format!(
            "desktop {id:?} changed after launch was requested; refusing to connect to an unverified endpoint"
        )));
    }
    let mut command = Command::new(&paths.real_herdr);
    for name in NESTED_ENVIRONMENT {
        command.env_remove(name);
    }
    command.env_remove("HERDR_DESKTOP_EXPECTED_PLAN");
    if let LaunchPlan::Remote {
        target,
        session,
        keybindings,
    } = plan
    {
        command
            .arg("--remote")
            .arg(target)
            .arg("--session")
            .arg(session)
            .arg("--remote-keybindings")
            .arg(authority_name(keybindings));
    }
    let error = command.exec();
    Err(LaunchError(format!(
        "could not execute {}: {error}",
        paths.real_herdr.display()
    )))
}

pub fn load_and_plan(paths: &RuntimePaths, id: &str) -> Result<LaunchPlan, LaunchError> {
    require_file(&paths.config, "desktop configuration")?;
    let config = load_path(&paths.config).map_err(|error| LaunchError(error.to_string()))?;
    plan_desktop(&config, id)
}

pub fn authority_name(authority: KeybindingAuthority) -> &'static str {
    match authority {
        KeybindingAuthority::Local => "local",
        KeybindingAuthority::Server => "server",
    }
}

fn preflight_remote(paths: &RuntimePaths, target: &str) -> Result<(), LaunchError> {
    require_executable(&paths.ssh, "SSH binary")?;
    let effective = run_command(
        &paths.ssh,
        &["-G".into(), target.into()],
        SSH_CONFIG_TIMEOUT,
    )
    .map_err(|error| {
        LaunchError(format!(
            "could not inspect effective SSH configuration for {target:?}: {error}"
        ))
    })?;
    validate_effective_ssh_config(target, &effective.stdout)?;
    run_command(&paths.ssh, &ssh_preflight_arguments(target), SSH_TIMEOUT)
        .map(|_| ())
        .map_err(|error| {
            LaunchError(format!(
                "remote desktop target {target:?} failed bounded non-interactive SSH preflight: {error}"
            ))
        })
}

fn ssh_preflight_arguments(target: &str) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "ConnectionAttempts=1".into(),
        "-o".into(),
        "ServerAliveInterval=3".into(),
        "-o".into(),
        "ServerAliveCountMax=1".into(),
        target.into(),
        "true".into(),
    ]
}

fn validate_effective_ssh_config(target: &str, output: &[u8]) -> Result<(), LaunchError> {
    let text = std::str::from_utf8(output).map_err(|_| {
        LaunchError(format!(
            "ssh -G returned non-UTF-8 configuration for {target:?}"
        ))
    })?;
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == key).then_some(value.trim())
        })
    };
    let positive_at_most = |key: &str, maximum: u32| {
        value(key)
            .and_then(|item| item.parse::<u32>().ok())
            .is_some_and(|item| item > 0 && item <= maximum)
    };
    let valid = value("batchmode") == Some("yes")
        && matches!(value("stricthostkeychecking"), Some("yes" | "true"))
        && positive_at_most("connecttimeout", 10)
        && value("connectionattempts") == Some("1")
        && positive_at_most("serveraliveinterval", 10)
        && positive_at_most("serveralivecountmax", 3);
    if !valid {
        return Err(LaunchError(format!(
            "SSH target {target:?} must have bounded non-interactive options in ~/.ssh/config before Herdr can use it: BatchMode yes, StrictHostKeyChecking yes, ConnectTimeout 5, ConnectionAttempts 1, ServerAliveInterval 3, ServerAliveCountMax 1"
        )));
    }
    Ok(())
}

fn launch_arguments(
    paths: &RuntimePaths,
    id: &str,
    expected_plan: &str,
    app: &Path,
    bundle_id: &str,
) -> Vec<String> {
    vec![
        "launch".into(),
        "--app".into(),
        app.to_string_lossy().into_owned(),
        "--bundle-id".into(),
        bundle_id.into(),
        "--command".into(),
        paths.command_shim.to_string_lossy().into_owned(),
        "--switcher-bin".into(),
        paths.switcher_bin.to_string_lossy().into_owned(),
        "--config".into(),
        paths.config.to_string_lossy().into_owned(),
        "--desktop-id".into(),
        id.into(),
        "--real-herdr".into(),
        paths.real_herdr.to_string_lossy().into_owned(),
        "--expected-plan".into(),
        expected_plan.into(),
    ]
}

fn identity_arguments(action: &str, record: &InstanceRecord) -> Vec<String> {
    vec![
        action.into(),
        "--pid".into(),
        record.pid.to_string(),
        "--app".into(),
        record.bundle_path.clone(),
        "--bundle-id".into(),
        record.bundle_id.clone(),
        "--launch-date-unix-ms".into(),
        record.launch_date_unix_ms.to_string(),
    ]
}

fn invoke_identity_helper(
    paths: &RuntimePaths,
    action: &str,
    record: &InstanceRecord,
) -> Result<Output, CommandFailure> {
    run_command(
        &paths.launcher,
        &identity_arguments(action, record),
        HELPER_TIMEOUT,
    )
}

fn parse_launch_identity(
    expected_app: &Path,
    expected_bundle_id: &str,
    stdout: &[u8],
) -> Result<LaunchIdentity, LaunchError> {
    let identity: LaunchIdentity = serde_json::from_slice(stdout)
        .map_err(|error| LaunchError(format!("invalid helper JSON: {error}")))?;
    if identity.pid <= 0
        || identity.launch_date_unix_ms <= 0
        || identity.bundle_id != expected_bundle_id
    {
        return Err(LaunchError(
            "helper returned invalid app identity fields".into(),
        ));
    }
    if Path::new(&identity.bundle_path) != expected_app {
        return Err(LaunchError(format!(
            "helper returned bundle path {:?}, expected {}",
            identity.bundle_path,
            expected_app.display()
        )));
    }
    Ok(identity)
}

fn record_from_desktop(desktop: &Desktop, identity: LaunchIdentity) -> InstanceRecord {
    let (mode, target, session, keybindings) = match &desktop.mode {
        DesktopMode::Local => ("local".into(), None, None, None),
        DesktopMode::Remote {
            target,
            session,
            keybindings,
            ..
        } => (
            "remote".into(),
            Some(target.clone()),
            Some(session.clone()),
            Some(authority_name(*keybindings).into()),
        ),
    };
    InstanceRecord {
        desktop_id: desktop.id.clone(),
        pid: identity.pid,
        launch_date_unix_ms: identity.launch_date_unix_ms,
        bundle_id: identity.bundle_id,
        bundle_path: identity.bundle_path,
        mode,
        target,
        session,
        keybindings,
    }
}

fn record_matches_desktop(
    record: &InstanceRecord,
    desktop: &Desktop,
    expected_app: &Path,
    expected_bundle_id: &str,
) -> bool {
    if record.bundle_id != expected_bundle_id || Path::new(&record.bundle_path) != expected_app {
        return false;
    }
    match &desktop.mode {
        DesktopMode::Local => {
            record.mode == "local"
                && record.target.is_none()
                && record.session.is_none()
                && record.keybindings.is_none()
        }
        DesktopMode::Remote {
            target,
            session,
            keybindings,
            ..
        } => {
            record.mode == "remote"
                && record.target.as_deref() == Some(target)
                && record.session.as_deref() == Some(session)
                && record.keybindings.as_deref() == Some(authority_name(*keybindings))
        }
    }
}

fn run_command(
    program: &Path,
    arguments: &[String],
    timeout: Duration,
) -> Result<Output, CommandFailure> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            CommandFailure::Start(format!("could not start {}: {error}", program.display()))
        })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CommandFailure::Timeout);
            }
            Err(error) => {
                return Err(CommandFailure::Wait(format!(
                    "could not wait for {}: {error}",
                    program.display()
                )));
            }
        }
    }
    let output = child.wait_with_output().map_err(|error| {
        CommandFailure::Wait(format!(
            "could not read {} output: {error}",
            program.display()
        ))
    })?;
    if !output.status.success() {
        return Err(CommandFailure::Exit {
            code: output.status.code(),
            stderr: bounded_text(&output.stderr),
        });
    }
    Ok(output)
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn require_absolute(path: &Path, label: &str) -> Result<(), LaunchError> {
    if !path.is_absolute() {
        return Err(LaunchError(format!(
            "{label} path must be absolute: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_file(path: &Path, label: &str) -> Result<(), LaunchError> {
    require_absolute(path, label)?;
    let metadata = fs::metadata(path).map_err(|error| {
        LaunchError(format!(
            "could not inspect {label} {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(LaunchError(format!(
            "{label} is not a file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_directory(path: &Path, label: &str) -> Result<(), LaunchError> {
    require_absolute(path, label)?;
    let metadata = fs::metadata(path).map_err(|error| {
        LaunchError(format!(
            "could not inspect {label} {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(LaunchError(format!(
            "{label} is not a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_executable(path: &Path, label: &str) -> Result<(), LaunchError> {
    require_file(path, label)?;
    if fs::metadata(path)
        .map_err(|error| {
            LaunchError(format!(
                "could not inspect {label} {}: {error}",
                path.display()
            ))
        })?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err(LaunchError(format!(
            "{label} is not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(1000)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_str;

    const CONFIG: &str = r#"
version = 1
default = "local"
[desktops.local]
mode = "local"
label = "Local"
[desktops.devbox]
mode = "remote"
label = "Devbox"
target = "dev"
session = "devbox"
keybindings = "server"
"#;

    fn paths() -> RuntimePaths {
        RuntimePaths {
            home: "/Users/tester".into(),
            app: "/Applications/Herdr.app".into(),
            command_shim: "/repo/scripts/herdr-desktop-command".into(),
            config: "/repo/desktops.toml".into(),
            launcher: "/bin/launcher".into(),
            real_herdr: "/bin/herdr".into(),
            ssh: "/usr/bin/ssh".into(),
            state_dir: "/state".into(),
            switcher_bin: "/bin/switcher".into(),
        }
    }

    #[test]
    fn plans_local_and_remote_argument_arrays() {
        let config = parse_str(CONFIG).expect("valid config");
        assert_eq!(
            plan_desktop(&config, "local").expect("local"),
            LaunchPlan::Local
        );
        assert!(matches!(
            plan_desktop(&config, "devbox").expect("remote"),
            LaunchPlan::Remote {
                target,
                session,
                keybindings: KeybindingAuthority::Server,
            } if target == "dev" && session == "devbox"
        ));
        let plan = plan_desktop(&config, "devbox").expect("remote plan");
        let encoded = serde_json::to_string(&plan).expect("encode plan");
        assert_eq!(
            serde_json::from_str::<LaunchPlan>(&encoded).expect("decode plan"),
            plan
        );
    }

    #[test]
    fn expected_bundle_ids_cover_local_and_remote_desktops() {
        let config = parse_str(CONFIG).expect("valid config");
        assert_eq!(
            expected_bundle_id(config.desktop("local").expect("local")),
            BUNDLE_ID
        );
        assert_eq!(
            expected_bundle_id(config.desktop("devbox").expect("devbox")),
            "com.gustavocaiano.herdr.devbox"
        );
    }

    #[test]
    fn expected_bundle_ids_hyphenate_underscores_in_desktop_ids() {
        let source = r#"
version = 1
default = "local"
[desktops.local]
mode = "local"
label = "Local"
[desktops.dev_box]
mode = "remote"
label = "Dev Box"
target = "dev"
session = "devbox"
keybindings = "local"
"#;
        let config = parse_str(source).expect("valid config");
        assert_eq!(
            expected_bundle_id(config.desktop("dev_box").expect("dev_box")),
            "com.gustavocaiano.herdr.dev-box"
        );
    }

    #[test]
    fn desktop_apps_resolve_per_mode_under_home_applications() {
        let config = parse_str(CONFIG).expect("valid config");
        let paths = paths();
        assert_eq!(
            desktop_app_path(&paths, config.desktop("local").expect("local")),
            PathBuf::from("/Applications/Herdr.app")
        );
        assert_eq!(
            desktop_app_path(&paths, config.desktop("devbox").expect("devbox")),
            PathBuf::from("/Users/tester/Applications/Devbox.app")
        );
        assert_eq!(
            remote_app_path(Path::new("/Users/tester"), "Herdr Builder"),
            PathBuf::from("/Users/tester/Applications/Herdr Builder.app")
        );
    }

    #[test]
    fn launch_arguments_carry_per_desktop_app_and_bundle_id() {
        let arguments = launch_arguments(
            &paths(),
            "devbox",
            "{\"mode\":\"remote\"}",
            Path::new("/Users/tester/Applications/Devbox.app"),
            "com.gustavocaiano.herdr.devbox",
        );
        assert_eq!(arguments[0], "launch");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--app", "/Users/tester/Applications/Devbox.app"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--bundle-id", "com.gustavocaiano.herdr.devbox"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--desktop-id", "devbox"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--expected-plan", "{\"mode\":\"remote\"}"])
        );
    }

    #[test]
    fn parses_only_expected_launch_identity() {
        let app = std::env::temp_dir().join(format!("herdr-app-{}", std::process::id()));
        fs::create_dir_all(&app).expect("create app dir");
        let canonical = fs::canonicalize(&app).expect("canonical app");
        let json = format!(
            "{{\"pid\":42,\"launch_date_unix_ms\":99,\"bundle_id\":\"{BUNDLE_ID}\",\"bundle_path\":{:?}}}",
            canonical.to_string_lossy()
        );
        assert_eq!(
            parse_launch_identity(&canonical, BUNDLE_ID, json.as_bytes())
                .expect("identity")
                .pid,
            42
        );
        assert!(parse_launch_identity(&canonical, BUNDLE_ID, b"{\"pid\":42}").is_err());

        let foreign = json.replace(&format!("\"{BUNDLE_ID}\""), "\"com.evil.app\"");
        assert!(parse_launch_identity(&canonical, BUNDLE_ID, foreign.as_bytes()).is_err());

        let relocated = format!(
            "{{\"pid\":42,\"launch_date_unix_ms\":99,\"bundle_id\":\"{BUNDLE_ID}\",\"bundle_path\":\"/Applications/Elsewhere.app\"}}"
        );
        assert!(parse_launch_identity(&canonical, BUNDLE_ID, relocated.as_bytes()).is_err());

        let devbox_id = "com.gustavocaiano.herdr.devbox";
        let remote_json = format!(
            "{{\"pid\":43,\"launch_date_unix_ms\":100,\"bundle_id\":\"{devbox_id}\",\"bundle_path\":{:?}}}",
            canonical.to_string_lossy()
        );
        assert_eq!(
            parse_launch_identity(&canonical, devbox_id, remote_json.as_bytes())
                .expect("per-desktop identity")
                .pid,
            43
        );
        fs::remove_dir(app).expect("cleanup app dir");
    }

    #[test]
    fn configuration_drift_is_detected() {
        let config = parse_str(CONFIG).expect("valid config");
        let local = config.desktop("local").expect("local");
        let local_identity = LaunchIdentity {
            pid: 1,
            launch_date_unix_ms: 1,
            bundle_id: expected_bundle_id(local),
            bundle_path: "/Applications/Herdr.app".into(),
        };
        let local_record = record_from_desktop(local, local_identity);
        assert!(record_matches_desktop(
            &local_record,
            local,
            Path::new("/Applications/Herdr.app"),
            BUNDLE_ID
        ));

        let devbox = config.desktop("devbox").expect("devbox");
        let devbox_id = expected_bundle_id(devbox);
        let identity = LaunchIdentity {
            pid: 1,
            launch_date_unix_ms: 1,
            bundle_id: devbox_id.clone(),
            bundle_path: "/Users/tester/Applications/Devbox.app".into(),
        };
        let mut record = record_from_desktop(devbox, identity);
        assert!(record_matches_desktop(
            &record,
            devbox,
            Path::new("/Users/tester/Applications/Devbox.app"),
            &devbox_id
        ));
        record.session = Some("changed".into());
        assert!(!record_matches_desktop(
            &record,
            devbox,
            Path::new("/Users/tester/Applications/Devbox.app"),
            &devbox_id
        ));
    }

    #[test]
    fn legacy_shared_bundle_records_trigger_drift_for_remote_desktops() {
        let config = parse_str(CONFIG).expect("valid config");
        let devbox = config.desktop("devbox").expect("devbox");
        let identity = LaunchIdentity {
            pid: 1,
            launch_date_unix_ms: 1,
            bundle_id: BUNDLE_ID.into(),
            bundle_path: "/Applications/Herdr.app".into(),
        };
        let record = record_from_desktop(devbox, identity);
        assert!(!record_matches_desktop(
            &record,
            devbox,
            Path::new("/Users/tester/Applications/Devbox.app"),
            "com.gustavocaiano.herdr.devbox"
        ));
    }

    #[test]
    fn relocated_bundle_paths_trigger_drift() {
        let config = parse_str(CONFIG).expect("valid config");
        let devbox = config.desktop("devbox").expect("devbox");
        let identity = LaunchIdentity {
            pid: 1,
            launch_date_unix_ms: 1,
            bundle_id: "com.gustavocaiano.herdr.devbox".into(),
            bundle_path: "/Applications/Herdr.app".into(),
        };
        let record = record_from_desktop(devbox, identity);
        assert!(!record_matches_desktop(
            &record,
            devbox,
            Path::new("/Users/tester/Applications/Devbox.app"),
            "com.gustavocaiano.herdr.devbox"
        ));
    }

    #[test]
    fn helper_arguments_include_recorded_identity() {
        let record = InstanceRecord {
            desktop_id: "local".into(),
            pid: 42,
            launch_date_unix_ms: 99,
            bundle_id: BUNDLE_ID.into(),
            bundle_path: "/Applications/Herdr.app".into(),
            mode: "local".into(),
            target: None,
            session: None,
            keybindings: None,
        };
        let arguments = identity_arguments("activate", &record);
        assert_eq!(arguments[0], "activate");
        assert!(arguments.windows(2).any(|pair| pair == ["--pid", "42"]));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--app", "/Applications/Herdr.app"])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--bundle-id", BUNDLE_ID])
        );
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--launch-date-unix-ms", "99"])
        );
    }

    #[test]
    fn ssh_preflight_arguments_are_bounded_and_noninteractive() {
        let arguments = vec![
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=5",
            "ConnectionAttempts=1",
            "ServerAliveInterval=3",
            "ServerAliveCountMax=1",
        ];
        let rendered = format!("{:?}", ssh_preflight_arguments("dev"));
        for argument in arguments {
            assert!(rendered.contains(argument));
        }
        assert!(rendered.contains("dev"));
    }

    #[test]
    fn effective_ssh_config_must_bound_the_real_herdr_transport() {
        let valid = b"batchmode yes\nstricthostkeychecking true\nconnectionattempts 1\nserveralivecountmax 1\nserveraliveinterval 3\nconnecttimeout 5\n";
        assert!(validate_effective_ssh_config("dev", valid).is_ok());

        for invalid in [
            b"batchmode no\nstricthostkeychecking true\nconnectionattempts 1\nserveralivecountmax 1\nserveraliveinterval 3\nconnecttimeout 5\n".as_slice(),
            b"batchmode yes\nstricthostkeychecking ask\nconnectionattempts 1\nserveralivecountmax 1\nserveraliveinterval 3\nconnecttimeout 5\n".as_slice(),
            b"batchmode yes\nstricthostkeychecking true\nconnectionattempts 1\nserveralivecountmax 1\nserveraliveinterval 3\nconnecttimeout none\n".as_slice(),
        ] {
            assert!(validate_effective_ssh_config("dev", invalid).is_err());
        }
    }
}
