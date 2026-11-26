//! The managed scope's `config.yaml` for security readers. The shared loader
//! skips a managed file it cannot use, with a warning, and the profile's values
//! then stand; these readers refuse instead, as the file may pin a restriction.

use std::path::Path;

/// The managed `config.yaml` in `dir` as the shared loader lays it over a
/// profile's (`${VAR}` references expanded, legacy keys normalised), or `None`
/// when it names nothing (missing, empty or only comments). A file the loader
/// would skip is an error.
pub fn managed_config_yaml(dir: &Path) -> Result<Option<serde_yaml::Value>, String> {
    let path = dir.join("config.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut root = match serde_yaml::from_str::<serde_yaml::Value>(&text) {
        Err(e) => return Err(format!("Invalid YAML in {}: {e}", path.display())),
        Ok(serde_yaml::Value::Null) => return Ok(None),
        Ok(root) => root,
    };
    crate::loader::expand_env_vars_in_yaml(&mut root);
    let serde_yaml::Value::Mapping(map) = &mut root else {
        return Err(format!("{} must be a mapping", path.display()));
    };
    crate::python_yaml_compat::normalize_config_yaml_root(map);
    serde_json::to_value(&root)
        .map_err(|e| format!("{} cannot be read as JSON: {e}", path.display()))?;
    Ok(Some(root))
}

/// Runs `read`, which lays the managed config in `dir` over a profile's through
/// the shared loader, refusing a managed file the loader would skip, or one
/// that changed during `read`, as the loader may then have skipped it.
pub fn with_managed_config<T>(dir: &Path, read: impl FnOnce() -> T) -> Result<T, String> {
    let before = managed_config_yaml(dir)?;
    let value = read();
    if managed_config_yaml(dir)? != before {
        return Err(format!(
            "{} changed while it was read",
            dir.join("config.yaml").display()
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), text).unwrap();
        dir
    }

    #[test]
    fn a_managed_config_naming_nothing_is_none() {
        assert_eq!(
            managed_config_yaml(tempfile::tempdir().unwrap().path()),
            Ok(None)
        );
        for text in ["", " \n", "# none yet\n", "~\n"] {
            assert_eq!(
                managed_config_yaml(managed(text).path()),
                Ok(None),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_managed_config_the_loader_would_skip_is_an_error() {
        for text in ["security: [unclosed\n", "- a list\n", "? [a, b]\n: x\n"] {
            assert!(
                managed_config_yaml(managed(text).path()).is_err(),
                "{text:?}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("config.yaml")).unwrap();
        assert!(managed_config_yaml(dir.path()).is_err());
    }

    #[test]
    fn a_managed_config_is_read_as_the_loader_reads_it() {
        let dir = managed("security:\n  tool_guard:\n    command: ${HERMES_TEST_MANAGED_BIN}\n");
        let _env = crate::managed_gateway::test_lock::lock();
        std::env::set_var("HERMES_TEST_MANAGED_BIN", "expanded.exe");
        let read = managed_config_yaml(dir.path());
        std::env::remove_var("HERMES_TEST_MANAGED_BIN");
        let root = read.unwrap().unwrap();
        assert_eq!(
            root["security"]["tool_guard"]["command"].as_str(),
            Some("expanded.exe")
        );
    }

    #[test]
    fn a_managed_config_that_changes_while_it_is_read_refuses() {
        let dir = managed("security: {}\n");
        let file = dir.path().join("config.yaml");
        assert_eq!(with_managed_config(dir.path(), || 1), Ok(1));
        for changed in [
            "security: [unclosed\n",
            "security:\n  website_blocklist:\n    enabled: true\n",
        ] {
            std::fs::write(&file, "security: {}\n").unwrap();
            let read = with_managed_config(dir.path(), || std::fs::write(&file, changed));
            assert!(read.is_err(), "{changed:?} went unnoticed");
        }
    }
}
