//! Public naming and the user-owned paths derived from it.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("horde")
}
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/horde")
}
pub fn project_config(project: &Path) -> PathBuf {
    project.join(".horde.toml")
}
pub fn templates(project: &Path) -> PathBuf {
    project.join(".horde/templates")
}
pub fn cli_name() -> &'static str {
    "horde"
}
pub fn install_dir(home: &Path) -> PathBuf {
    home.join(".local/share/horde-install")
}
