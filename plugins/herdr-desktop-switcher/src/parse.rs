use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConfig {
    pub version: u32,
    pub default: String,
    pub desktops: BTreeMap<String, Desktop>,
}

impl DesktopConfig {
    pub fn desktop(&self, id: &str) -> Option<&Desktop> {
        self.desktops.get(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desktop {
    pub id: String,
    pub label: String,
    pub mode: DesktopMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopMode {
    Local,
    Remote {
        target: String,
        session: String,
        keybindings: KeybindingAuthority,
        app_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeybindingAuthority {
    Local,
    Server,
}

#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Toml {
        context: String,
        source: toml::de::Error,
    },
    Validation(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Toml { context, source } => {
                write!(formatter, "invalid TOML in {context}: {source}")
            }
            Self::Validation(message) => formatter.write_str(message),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Toml { source, .. } => Some(source),
            Self::Validation(_) => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    version: u32,
    default: String,
    desktops: BTreeMap<String, RawDesktop>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDesktop {
    mode: RawMode,
    label: String,
    target: Option<String>,
    session: Option<String>,
    keybindings: Option<KeybindingAuthority>,
    app_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawMode {
    Local,
    Remote,
}

pub fn parse_str(source: &str) -> Result<DesktopConfig, ConfigError> {
    parse_with_context(source, "desktop configuration")
}

pub fn load_path(path: &Path) -> Result<DesktopConfig, ConfigError> {
    let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    parse_with_context(&source, &path.display().to_string())
}

fn parse_with_context(source: &str, context: &str) -> Result<DesktopConfig, ConfigError> {
    let raw: RawConfig = toml::from_str(source).map_err(|source| ConfigError::Toml {
        context: context.to_owned(),
        source,
    })?;

    validate(raw)
}

fn validate(raw: RawConfig) -> Result<DesktopConfig, ConfigError> {
    if raw.version != CONFIG_VERSION {
        return Err(validation(format!(
            "unsupported desktop configuration version {}; expected {CONFIG_VERSION}",
            raw.version
        )));
    }
    if raw.desktops.is_empty() {
        return Err(validation(
            "desktop configuration must define at least one desktop",
        ));
    }
    validate_desktop_id(&raw.default).map_err(|reason| {
        validation(format!(
            "invalid default desktop id {:?}: {reason}",
            raw.default
        ))
    })?;
    if !raw.desktops.contains_key(&raw.default) {
        return Err(validation(format!(
            "default desktop {:?} is not defined",
            raw.default
        )));
    }

    let mut desktops = BTreeMap::new();
    for (id, desktop) in raw.desktops {
        validate_desktop_id(&id)
            .map_err(|reason| validation(format!("invalid desktop id {id:?}: {reason}")))?;
        let RawDesktop {
            mode,
            label,
            target,
            session,
            keybindings,
            app_name,
        } = desktop;
        let label = validate_label(&id, label)?;
        let mode = match mode {
            RawMode::Local => validate_local(&id, target, session, keybindings, app_name)?,
            RawMode::Remote => {
                validate_remote(&id, &label, target, session, keybindings, app_name)?
            }
        };

        desktops.insert(id.clone(), Desktop { id, label, mode });
    }

    let mut app_name_owners = BTreeMap::new();
    for desktop in desktops.values() {
        if let DesktopMode::Remote { app_name, .. } = &desktop.mode
            && let Some(previous) = app_name_owners.insert(app_name.as_str(), desktop.id.as_str())
        {
            return Err(validation(format!(
                "remote desktops {previous:?} and {:?} both resolve to app name {app_name:?}; app names must be unique",
                desktop.id
            )));
        }
    }

    Ok(DesktopConfig {
        version: raw.version,
        default: raw.default,
        desktops,
    })
}

fn validate_local(
    id: &str,
    target: Option<String>,
    session: Option<String>,
    keybindings: Option<KeybindingAuthority>,
    app_name: Option<String>,
) -> Result<DesktopMode, ConfigError> {
    let mut forbidden = Vec::new();
    if target.is_some() {
        forbidden.push("target");
    }
    if session.is_some() {
        forbidden.push("session");
    }
    if keybindings.is_some() {
        forbidden.push("keybindings");
    }
    if app_name.is_some() {
        forbidden.push("app_name");
    }
    if !forbidden.is_empty() {
        return Err(validation(format!(
            "local desktop {id:?} must not define {}",
            forbidden.join(", ")
        )));
    }
    Ok(DesktopMode::Local)
}

fn validate_remote(
    id: &str,
    label: &str,
    target: Option<String>,
    session: Option<String>,
    keybindings: Option<KeybindingAuthority>,
    app_name: Option<String>,
) -> Result<DesktopMode, ConfigError> {
    let target = required_remote_field(id, "target", target)?;
    validate_target(&target).map_err(|reason| {
        validation(format!(
            "remote desktop {id:?} has invalid target {target:?}: {reason}"
        ))
    })?;

    let session = required_remote_field(id, "session", session)?;
    validate_session(&session).map_err(|reason| {
        validation(format!(
            "remote desktop {id:?} has invalid session {session:?}: {reason}"
        ))
    })?;

    let keybindings = keybindings.ok_or_else(|| {
        validation(format!(
            "remote desktop {id:?} must define keybindings as local or server"
        ))
    })?;

    let app_name = app_name.unwrap_or_else(|| label.to_owned());
    validate_app_name(&app_name).map_err(|reason| {
        validation(format!(
            "remote desktop {id:?} has invalid app name {app_name:?}: {reason}"
        ))
    })?;

    Ok(DesktopMode::Remote {
        target,
        session,
        keybindings,
        app_name,
    })
}

fn required_remote_field(
    id: &str,
    name: &str,
    value: Option<String>,
) -> Result<String, ConfigError> {
    value.ok_or_else(|| validation(format!("remote desktop {id:?} must define {name}")))
}

fn validate_desktop_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 64 {
        return Err("use between 1 and 64 ASCII characters");
    }
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty id");
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err("start with a lowercase ASCII letter or digit");
    }
    if !chars.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'
            || character == '_'
    }) {
        return Err("use only lowercase ASCII letters, digits, '-' and '_'");
    }
    Ok(())
}

pub(crate) fn is_valid_desktop_id(value: &str) -> bool {
    validate_desktop_id(value).is_ok()
}

fn validate_label(id: &str, label: String) -> Result<String, ConfigError> {
    if label.is_empty() || label.chars().count() > 80 {
        return Err(validation(format!(
            "desktop {id:?} label must contain between 1 and 80 characters"
        )));
    }
    if label.trim() != label {
        return Err(validation(format!(
            "desktop {id:?} label must not have leading or trailing whitespace"
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(validation(format!(
            "desktop {id:?} label must not contain control characters"
        )));
    }
    Ok(label)
}

fn validate_target(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 255 {
        return Err("use between 1 and 255 ASCII characters");
    }
    if value.starts_with('-') {
        return Err("must not begin with '-' because SSH would treat it as an option");
    }
    if value.contains("..") {
        return Err("must not contain '..'");
    }
    if !value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character == '.'
            || character == '_'
            || character == '-'
            || character == '@'
    }) {
        return Err("use only ASCII letters, digits, '.', '_', '-', and one optional '@'");
    }
    if value.matches('@').count() > 1 || value.starts_with('@') || value.ends_with('@') {
        return Err("use either an SSH alias/hostname or user@host");
    }
    Ok(())
}

fn validate_session(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 64 {
        return Err("use between 1 and 64 ASCII characters");
    }
    if value.starts_with('-') {
        return Err("must not begin with '-'");
    }
    if value.contains("..") {
        return Err("must not contain '..'");
    }
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty session");
    if !first.is_ascii_alphanumeric() {
        return Err("start with an ASCII letter or digit");
    }
    if !chars.all(|character| {
        character.is_ascii_alphanumeric()
            || character == '.'
            || character == '_'
            || character == '-'
    }) {
        return Err("use only ASCII letters, digits, '.', '_', and '-'");
    }
    Ok(())
}

fn validate_app_name(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.chars().count() > 64 {
        return Err("use between 1 and 64 characters");
    }
    if value.trim() != value {
        return Err("must not have leading or trailing whitespace");
    }
    if value.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    if value.starts_with('.') || value.starts_with('-') {
        return Err("must not begin with '.' or '-'");
    }
    if value.contains('/') {
        return Err("must not contain '/'");
    }
    if value.contains("..") {
        return Err("must not contain '..'");
    }
    Ok(())
}

fn validation(message: impl Into<String>) -> ConfigError {
    ConfigError::Validation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const VALID: &str = r#"
version = 1
default = "local"

[desktops.local]
mode = "local"
label = "Local"

[desktops.devbox]
mode = "remote"
label = "Devbox"
target = "developer@dev.example.com"
session = "devbox"
keybindings = "local"

[desktops.builder]
mode = "remote"
label = "Builder"
app_name = "Herdr Builder"
target = "builder@build.example.com"
session = "builder"
keybindings = "server"
"#;

    fn error(source: &str) -> String {
        parse_str(source)
            .expect_err("configuration should fail")
            .to_string()
    }

    fn one_desktop(body: &str) -> String {
        format!("version = 1\ndefault = \"local\"\n\n[desktops.local]\n{body}\n")
    }

    #[test]
    fn parses_valid_local_and_remote_desktops() {
        let config = parse_str(VALID).expect("valid configuration");
        assert_eq!(config.version, 1);
        assert_eq!(config.default, "local");
        assert_eq!(config.desktops.len(), 3);
        assert!(matches!(
            config.desktop("local").map(|desktop| &desktop.mode),
            Some(DesktopMode::Local)
        ));
        assert!(matches!(
            config.desktop("devbox").map(|desktop| &desktop.mode),
            Some(DesktopMode::Remote {
                target,
                session,
                app_name,
                keybindings: KeybindingAuthority::Local,
            }) if target == "developer@dev.example.com"
                && session == "devbox"
                && app_name == "Devbox"
        ));
        assert!(matches!(
            config.desktop("builder").map(|desktop| &desktop.mode),
            Some(DesktopMode::Remote {
                app_name,
                keybindings: KeybindingAuthority::Server,
                ..
            }) if app_name == "Herdr Builder"
        ));
    }

    #[test]
    fn parses_more_than_five_remotes_deterministically() {
        let mut source = String::from("version = 1\ndefault = \"local\"\n");
        source.push_str("[desktops.local]\nmode = \"local\"\nlabel = \"Local\"\n");
        for index in 0..6 {
            source.push_str(&format!(
                "[desktops.remote{index}]\nmode = \"remote\"\nlabel = \"Remote {index}\"\ntarget = \"remote{index}\"\nsession = \"session{index}\"\nkeybindings = \"server\"\n"
            ));
        }
        let config = parse_str(&source).expect("valid multi-remote configuration");
        assert_eq!(config.desktops.len(), 7);
        assert_eq!(
            config.desktops.keys().next().map(String::as_str),
            Some("local")
        );
    }

    #[test]
    fn rejects_missing_top_level_fields() {
        assert!(
            error("default = \"local\"\n[desktops.local]\nmode = \"local\"\nlabel = \"Local\"")
                .contains("version")
        );
        assert!(
            error("version = 1\n[desktops.local]\nmode = \"local\"\nlabel = \"Local\"")
                .contains("default")
        );
        assert!(error("version = 1\ndefault = \"local\"").contains("desktops"));
    }

    #[test]
    fn rejects_unsupported_version() {
        assert!(error(&VALID.replace("version = 1", "version = 2")).contains("unsupported"));
    }

    #[test]
    fn rejects_empty_desktops_and_unknown_default() {
        assert!(error("version = 1\ndefault = \"local\"\n[desktops]").contains("at least one"));
        assert!(
            error(&VALID.replace("default = \"local\"", "default = \"missing\""))
                .contains("not defined")
        );
    }

    #[test]
    fn rejects_duplicate_keys_and_tables() {
        let duplicate_key = one_desktop("mode = \"local\"\nlabel = \"Local\"\nlabel = \"Again\"");
        assert!(error(&duplicate_key).contains("invalid TOML"));

        let duplicate_table =
            format!("{VALID}\n[desktops.local]\nmode = \"local\"\nlabel = \"Again\"\n");
        assert!(error(&duplicate_table).contains("invalid TOML"));
    }

    #[test]
    fn rejects_malformed_and_unknown_toml() {
        assert!(error("version = [").contains("invalid TOML"));
        assert!(error(&format!("{VALID}\nextra = true\n")).contains("unknown field"));
        assert!(
            error(&one_desktop(
                "mode = \"local\"\nlabel = \"Local\"\ncommand = \"touch /tmp/pwn\""
            ))
            .contains("unknown field")
        );
        assert!(
            error(&one_desktop("mode = \"teleport\"\nlabel = \"Local\""))
                .contains("unknown variant")
        );
    }

    #[test]
    fn local_desktops_reject_remote_fields() {
        for field in [
            "target = \"dev\"",
            "session = \"devbox\"",
            "keybindings = \"local\"",
            "app_name = \"Devbox\"",
        ] {
            let source = one_desktop(&format!("mode = \"local\"\nlabel = \"Local\"\n{field}"));
            assert!(error(&source).contains("must not define"));
        }
    }

    #[test]
    fn remote_desktops_require_every_remote_field() {
        let fields = [
            ("target", "session = \"devbox\"\nkeybindings = \"local\""),
            ("session", "target = \"dev\"\nkeybindings = \"local\""),
            ("keybindings", "target = \"dev\"\nsession = \"devbox\""),
        ];
        for (missing, body) in fields {
            let source = one_desktop(&format!("mode = \"remote\"\nlabel = \"Devbox\"\n{body}"));
            assert!(error(&source).contains(missing));
        }
    }

    #[test]
    fn rejects_invalid_keybinding_authority() {
        let source = one_desktop(
            "mode = \"remote\"\nlabel = \"Devbox\"\ntarget = \"dev\"\nsession = \"devbox\"\nkeybindings = \"shared\"",
        );
        assert!(error(&source).contains("unknown variant"));
    }

    #[test]
    fn rejects_hostile_desktop_ids() {
        for id in [
            "../evil",
            "-option",
            "UPPER",
            "has space",
            "semi;colon",
            ".",
        ] {
            let source = format!(
                "version = 1\ndefault = {id:?}\n[desktops.{id:?}]\nmode = \"local\"\nlabel = \"Local\"\n"
            );
            assert!(
                error(&source).contains("invalid default desktop id"),
                "id={id}"
            );
        }
    }

    #[test]
    fn rejects_hostile_targets() {
        for target in [
            "-oProxyCommand=evil",
            "../dev",
            "dev host",
            "dev;touch",
            "$(command)",
            "user@@host",
            "@host",
        ] {
            let source = one_desktop(&format!(
                "mode = \"remote\"\nlabel = \"Devbox\"\ntarget = {target:?}\nsession = \"devbox\"\nkeybindings = \"local\""
            ));
            assert!(error(&source).contains("invalid target"), "target={target}");
        }
    }

    #[test]
    fn rejects_hostile_sessions() {
        for session in ["-option", "../devbox", "dev box", "dev;touch", "$(command)"] {
            let source = one_desktop(&format!(
                "mode = \"remote\"\nlabel = \"Devbox\"\ntarget = \"dev\"\nsession = {session:?}\nkeybindings = \"local\""
            ));
            assert!(
                error(&source).contains("invalid session"),
                "session={session}"
            );
        }
    }

    #[test]
    fn rejects_hostile_app_names() {
        for name in [
            "",
            " Devbox",
            "Devbox ",
            ".Devbox",
            "-Devbox",
            "Dev/box",
            "Dev..box",
            "dev\\u0001box",
        ] {
            let source = one_desktop(&format!(
                "mode = \"remote\"\nlabel = \"Devbox\"\ntarget = \"dev\"\nsession = \"devbox\"\nkeybindings = \"local\"\napp_name = \"{name}\""
            ));
            assert!(
                error(&source).contains("invalid app name"),
                "app_name={name:?}"
            );
        }
        let long_name = "x".repeat(65);
        let source = one_desktop(&format!(
            "mode = \"remote\"\nlabel = \"Devbox\"\ntarget = \"dev\"\nsession = \"devbox\"\nkeybindings = \"local\"\napp_name = \"{long_name}\""
        ));
        assert!(error(&source).contains("invalid app name"));
    }

    #[test]
    fn rejects_duplicate_effective_app_names() {
        let same_label = format!(
            "{VALID}\n[desktops.twin]\nmode = \"remote\"\nlabel = \"Devbox\"\ntarget = \"twin\"\nsession = \"twin\"\nkeybindings = \"server\"\n"
        );
        assert!(error(&same_label).contains("must be unique"));

        let explicit = format!(
            "{VALID}\n[desktops.clone]\nmode = \"remote\"\nlabel = \"Clone\"\napp_name = \"Devbox\"\ntarget = \"clone\"\nsession = \"clone\"\nkeybindings = \"server\"\n"
        );
        assert!(error(&explicit).contains("must be unique"));
    }

    #[test]
    fn rejects_invalid_labels() {
        for label in ["", " Local", "Local ", "\u{1}"] {
            let source = one_desktop(&format!("mode = \"local\"\nlabel = {label:?}"));
            assert!(error(&source).contains("label"), "label={label:?}");
        }
        let long_label = "x".repeat(81);
        let source = one_desktop(&format!("mode = \"local\"\nlabel = {long_label:?}"));
        assert!(error(&source).contains("label"));
    }

    #[test]
    fn reports_load_path_errors_and_loads_valid_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-desktop-switcher-{}-{unique}.toml",
            std::process::id()
        ));

        let missing = load_path(&path).expect_err("missing path should fail");
        assert!(matches!(missing, ConfigError::Read { .. }));
        assert!(missing.to_string().contains(&path.display().to_string()));

        fs::write(&path, VALID).expect("write temporary config");
        let loaded = load_path(&path).expect("load temporary config");
        fs::remove_file(&path).expect("remove temporary config");
        assert_eq!(loaded.default, "local");
    }
}
