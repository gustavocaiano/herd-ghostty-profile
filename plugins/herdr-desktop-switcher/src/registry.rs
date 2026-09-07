use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::parse::is_valid_desktop_id;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRecord {
    pub desktop_id: String,
    pub pid: i32,
    pub launch_date_unix_ms: i64,
    pub bundle_id: String,
    pub bundle_path: String,
    pub mode: String,
    pub target: Option<String>,
    pub session: Option<String>,
    pub keybindings: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    root: PathBuf,
}

#[derive(Debug)]
pub struct RegistryError(String);

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for RegistryError {}

pub struct DesktopLock {
    file: File,
}

impl Drop for DesktopLock {
    fn drop(&mut self) {
        // SAFETY: file is an open descriptor owned by this guard. Unlocking it
        // cannot affect memory safety; the OS also releases it when file drops.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Registry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lock(&self, desktop_id: &str, timeout: Duration) -> Result<DesktopLock, RegistryError> {
        self.ensure_root()?;
        let path = self.component_path(desktop_id, "lock")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|error| io_error("open lock", &path, error))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("set permissions on", &path, error))?;

        let started = Instant::now();
        loop {
            // SAFETY: flock acts only on this valid open descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(DesktopLock { file });
            }
            let error = io::Error::last_os_error();
            let code = error.raw_os_error();
            if code != Some(libc::EWOULDBLOCK) && code != Some(libc::EAGAIN) {
                return Err(io_error("lock", &path, error));
            }
            if started.elapsed() >= timeout {
                return Err(RegistryError(format!(
                    "timed out waiting for desktop {desktop_id:?} launch lock"
                )));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn write(&self, record: &InstanceRecord) -> Result<(), RegistryError> {
        validate_record(record)?;
        self.ensure_root()?;
        let destination = self.component_path(&record.desktop_id, "toml")?;
        let temporary = self.temporary_path(&record.desktop_id)?;
        let encoded = toml::to_string(record)
            .map_err(|error| RegistryError(format!("could not encode instance record: {error}")))?;

        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| io_error("create", &temporary, error))?;
            file.write_all(encoded.as_bytes())
                .map_err(|error| io_error("write", &temporary, error))?;
            file.sync_all()
                .map_err(|error| io_error("sync", &temporary, error))?;
            fs::rename(&temporary, &destination)
                .map_err(|error| io_error("replace", &destination, error))?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
                .map_err(|error| io_error("set permissions on", &destination, error))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn remove(&self, desktop_id: &str) -> Result<(), RegistryError> {
        if !self.existing_root()? {
            return Ok(());
        }
        let path = self.component_path(desktop_id, "toml")?;
        remove_file_if_present(&path, "remove instance record")
    }

    pub fn mark_pending(&self, desktop_id: &str) -> Result<(), RegistryError> {
        self.ensure_root()?;
        let path = self.component_path(desktop_id, "pending")?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(RegistryError(format!(
                    "desktop {desktop_id:?} has an unresolved previous launch; inspect {} before retrying",
                    path.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("inspect", &path, error)),
        }
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| RegistryError(format!("system clock is before Unix epoch: {error}")))?
            .as_secs();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| io_error("create pending marker", &path, error))?;
        writeln!(file, "started_at_unix = {started}")
            .map_err(|error| io_error("write pending marker", &path, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync pending marker", &path, error))?;
        Ok(())
    }

    pub fn pending(&self, desktop_id: &str) -> Result<bool, RegistryError> {
        if !self.existing_root()? {
            return Ok(false);
        }
        let path = self.component_path(desktop_id, "pending")?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                Err(RegistryError(format!(
                    "pending marker {} must be a regular file",
                    path.display()
                )))
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error("inspect", &path, error)),
        }
    }

    pub fn clear_pending(&self, desktop_id: &str) -> Result<(), RegistryError> {
        if !self.existing_root()? {
            return Ok(());
        }
        let path = self.component_path(desktop_id, "pending")?;
        remove_file_if_present(&path, "remove pending marker")
    }

    pub fn prune(&self) -> Result<Vec<InstanceRecord>, RegistryError> {
        if !self.existing_root()? {
            return Ok(Vec::new());
        }
        let mut live = Vec::new();
        let entries = fs::read_dir(&self.root)
            .map_err(|error| io_error("read directory", &self.root, error))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                RegistryError(format!(
                    "could not read an entry in {}: {error}",
                    self.root.display()
                ))
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|error| io_error("inspect", &path, error))?;
            if file_type.is_symlink() || !file_type.is_file() {
                remove_file_if_present(&path, "remove unsafe registry entry")?;
                continue;
            }

            let stem = path.file_stem().and_then(|value| value.to_str());
            let record = read_record(&path).ok();
            let valid = record.as_ref().is_some_and(|record| {
                stem == Some(record.desktop_id.as_str())
                    && validate_record(record).is_ok()
                    && process_alive(record.pid)
            });
            if valid {
                live.push(record.expect("record checked above"));
            } else {
                remove_file_if_present(&path, "remove stale registry entry")?;
            }
        }
        live.sort_by(|left, right| left.desktop_id.cmp(&right.desktop_id));
        Ok(live)
    }

    pub fn live_for(&self, desktop_id: &str) -> Result<Option<InstanceRecord>, RegistryError> {
        require_valid_id(desktop_id)?;
        Ok(self
            .prune()?
            .into_iter()
            .find(|record| record.desktop_id == desktop_id))
    }

    fn ensure_root(&self) -> Result<(), RegistryError> {
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(real_directory_error(&self.root));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(&self.root)
                .map_err(|error| io_error("create directory", &self.root, error))?,
            Err(error) => return Err(io_error("inspect", &self.root, error)),
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("set permissions on", &self.root, error))
    }

    fn existing_root(&self) -> Result<bool, RegistryError> {
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(real_directory_error(&self.root))
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error("inspect", &self.root, error)),
        }
    }

    fn component_path(&self, desktop_id: &str, extension: &str) -> Result<PathBuf, RegistryError> {
        require_valid_id(desktop_id)?;
        Ok(self.root.join(format!("{desktop_id}.{extension}")))
    }

    fn temporary_path(&self, desktop_id: &str) -> Result<PathBuf, RegistryError> {
        require_valid_id(desktop_id)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| RegistryError(format!("system clock is before Unix epoch: {error}")))?
            .as_nanos();
        Ok(self
            .root
            .join(format!(".{desktop_id}.{}.{nonce}.tmp", std::process::id())))
    }
}

fn require_valid_id(desktop_id: &str) -> Result<(), RegistryError> {
    if !is_valid_desktop_id(desktop_id) {
        return Err(RegistryError(format!("invalid desktop id {desktop_id:?}")));
    }
    Ok(())
}

fn validate_record(record: &InstanceRecord) -> Result<(), RegistryError> {
    require_valid_id(&record.desktop_id)?;
    if record.pid <= 0 || record.launch_date_unix_ms <= 0 {
        return Err(RegistryError(
            "instance record PID and launch date must be positive".into(),
        ));
    }
    if !is_family_bundle_id(&record.bundle_id) || !Path::new(&record.bundle_path).is_absolute() {
        return Err(RegistryError(
            "instance record has invalid app identity".into(),
        ));
    }
    match record.mode.as_str() {
        "local"
            if record.target.is_none()
                && record.session.is_none()
                && record.keybindings.is_none() => {}
        "remote"
            if record.target.is_some()
                && record.session.is_some()
                && matches!(record.keybindings.as_deref(), Some("local" | "server")) => {}
        _ => {
            return Err(RegistryError(
                "instance record has inconsistent desktop configuration".into(),
            ));
        }
    }
    Ok(())
}

fn is_family_bundle_id(bundle_id: &str) -> bool {
    bundle_id == "com.gustavocaiano.herdr" || bundle_id.starts_with("com.gustavocaiano.herdr.")
}

fn read_record(path: &Path) -> Result<InstanceRecord, RegistryError> {
    let source = fs::read_to_string(path).map_err(|error| io_error("read", path, error))?;
    toml::from_str(&source).map_err(|error| {
        RegistryError(format!(
            "invalid registry record {}: {error}",
            path.display()
        ))
    })
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs only a liveness/permission check.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn remove_file_if_present(path: &Path, action: &str) -> Result<(), RegistryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(action, path, error)),
    }
}

fn real_directory_error(path: &Path) -> RegistryError {
    RegistryError(format!(
        "registry path {} must be a real directory",
        path.display()
    ))
}

fn io_error(action: &str, path: &Path, error: io::Error) -> RegistryError {
    RegistryError(format!("could not {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-desktop-registry-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn live_record(id: &str) -> InstanceRecord {
        InstanceRecord {
            desktop_id: id.into(),
            pid: i32::try_from(std::process::id()).expect("PID fits i32"),
            launch_date_unix_ms: 1,
            bundle_id: "com.gustavocaiano.herdr".into(),
            bundle_path: "/Applications/Herdr.app".into(),
            mode: "local".into(),
            target: None,
            session: None,
            keybindings: None,
        }
    }

    #[test]
    fn round_trips_live_records_with_private_permissions() {
        let root = temp_root("roundtrip");
        let registry = Registry::new(&root);
        registry.write(&live_record("local")).expect("write record");
        assert_eq!(registry.prune().expect("prune"), vec![live_record("local")]);
        assert_eq!(
            fs::metadata(&root).expect("root").permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join("local.toml"))
                .expect("record")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn prunes_stale_malformed_and_symlink_records() {
        let root = temp_root("prune");
        let registry = Registry::new(&root);
        registry.write(&live_record("local")).expect("write live");
        let mut stale = live_record("stale");
        stale.pid = i32::MAX;
        registry.write(&stale).expect("write stale");
        fs::write(root.join("broken.toml"), "not = [valid").expect("write malformed");
        std::os::unix::fs::symlink("local.toml", root.join("linked.toml"))
            .expect("create record symlink");

        let live = registry.prune().expect("prune");
        assert_eq!(live.len(), 1);
        assert!(!root.join("stale.toml").exists());
        assert!(!root.join("broken.toml").exists());
        assert!(!root.join("linked.toml").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn family_bundle_ids_are_accepted_and_foreign_apps_are_rejected() {
        let root = temp_root("bundle-ids");
        let registry = Registry::new(&root);

        let mut remote = live_record("devbox");
        remote.bundle_id = "com.gustavocaiano.herdr.devbox".into();
        remote.bundle_path = "/Users/tester/Applications/Devbox.app".into();
        remote.mode = "remote".into();
        remote.target = Some("dev".into());
        remote.session = Some("devbox".into());
        remote.keybindings = Some("local".into());
        registry
            .write(&remote)
            .expect("write per-desktop app record");
        assert_eq!(registry.prune().expect("prune live records"), vec![remote]);

        for hostile in ["com.evil.app", "com.gustavocaiano.herdrdotnet"] {
            let mut foreign = live_record("local");
            foreign.bundle_id = hostile.into();
            assert!(
                registry.write(&foreign).is_err(),
                "bundle_id={hostile} must be rejected"
            );
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn pending_marker_blocks_repeated_launch_until_cleared() {
        let root = temp_root("pending");
        let registry = Registry::new(&root);
        registry.mark_pending("local").expect("mark pending");
        assert!(registry.pending("local").expect("pending"));
        assert!(registry.mark_pending("local").is_err());
        registry.clear_pending("local").expect("clear pending");
        assert!(!registry.pending("local").expect("not pending"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn desktop_lock_serializes_same_id() {
        let root = temp_root("lock");
        let registry = Registry::new(&root);
        let first = registry
            .lock("local", Duration::from_millis(50))
            .expect("first lock");
        assert!(registry.lock("local", Duration::from_millis(50)).is_err());
        drop(first);
        assert!(registry.lock("local", Duration::from_millis(50)).is_ok());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rejects_unsafe_ids_and_symlink_roots_for_all_operations() {
        let root = temp_root("unsafe");
        let registry = Registry::new(&root);
        assert!(registry.live_for("../escape").is_err());
        let real = temp_root("real");
        fs::create_dir(&real).expect("create real root");
        std::os::unix::fs::symlink(&real, &root).expect("create root symlink");
        assert!(registry.prune().is_err());
        assert!(registry.remove("local").is_err());
        assert!(registry.clear_pending("local").is_err());
        fs::remove_file(root).expect("remove root symlink");
        fs::remove_dir(real).expect("remove real root");
    }
}
