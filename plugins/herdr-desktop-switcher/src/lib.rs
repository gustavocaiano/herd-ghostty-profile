pub mod api;
pub mod launch;
pub mod parse;
pub mod pick;
pub mod registry;

pub use launch::{LaunchOutcome, LaunchPlan, RuntimePaths, launch_desktop, plan_desktop};
pub use parse::{
    ConfigError, Desktop, DesktopConfig, DesktopMode, KeybindingAuthority, load_path, parse_str,
};
pub use registry::{InstanceRecord, Registry};
