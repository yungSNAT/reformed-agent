use std::path::{Path, PathBuf};

/// The home `--profile <name>` selects, as upstream Hermes resolves it (`resolve_profile_env`):
/// the root home for `default`, else the existing folder `<root>/profiles/<name>`. The root is
/// the current home, or its grandparent when the current home is itself a profile's folder.
pub fn profile_home(name: &str) -> Result<PathBuf, String> {
    let current = hermes_config::hermes_home();
    let root = match current.parent() {
        Some(parent) if parent.file_name() == Some("profiles".as_ref()) => {
            parent.parent().unwrap_or(parent).to_path_buf()
        }
        _ => current,
    };
    let canon = name.trim().to_ascii_lowercase();
    if canon == "default" {
        return Ok(root);
    }
    // Upstream's PROFILE_ID_RE, `^[a-z0-9][a-z0-9_-]{0,63}$`: the name is a path component.
    let valid = canon.len() <= 64
        && canon.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && canon
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !valid {
        return Err(format!(
            "Invalid profile name '{name}': use up to 64 lowercase letters, digits, '_' and '-', starting with a letter or digit"
        ));
    }
    let home = root.join("profiles").join(&canon);
    if home.is_dir() {
        Ok(home)
    } else {
        Err(format!(
            "Profile '{canon}' does not exist: no folder {}",
            home.display()
        ))
    }
}

pub fn list_profiles(config_dir: &Path) -> Vec<String> {
    let profiles = config_dir.join("profiles");
    let Ok(entries) = std::fs::read_dir(profiles) else {
        return vec![];
    };
    entries
        .flatten()
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect()
}
