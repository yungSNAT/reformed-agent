//! Runtime key-override tracking and `.env` snapshot readers for `chat::model`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use hermes_config::loader::{dotenv_assignment, env_name_key, env_value_round_trips, strip_quotes};

/// The project `.env`'s folder: the working folder, as start-up reads it.
pub(super) const PROJECT: &str = "";

/// Key variables changed since start-up by `model.save_key`, `model.disconnect` or
/// `reload.env`; `None` for a removed one. A turn and the picker read these before the process
/// environment, which only start-up sets.
#[derive(Default)]
pub(in crate::chat) struct Keys {
    pub(super) changed: Mutex<HashMap<String, Option<String>>>,
    /// The names this gateway has seen in the home `.env` (at start-up, on a save or a reload),
    /// as [`env_name_key`] gives them. Not the project `.env`: a reload reads that one only to
    /// fill gaps, so a key it holds may be missing from what a reload reads.
    pub(super) from_file: Mutex<BTreeSet<String>>,
    /// The names in the home `.env` at start-up, as [`env_name_key`] gives them.
    pub(super) started_in_home_file: BTreeSet<String>,
    /// The names in the project `.env` (the working folder's) at start-up, likewise.
    pub(super) started_in_project_file: BTreeSet<String>,
}

/// The assignments in `dir`'s `.env` as start-up's fill-only project read takes them (the
/// first of 2 lines), names as [`env_name_key`] gives them: none for a missing file, `None`
/// for one that cannot be read.
pub(super) fn env_file_values(dir: &Path) -> Option<HashMap<String, String>> {
    match std::fs::read_to_string(dir.join(".env")) {
        Ok(contents) => {
            let mut values = HashMap::new();
            for (name, value) in contents.lines().filter_map(dotenv_assignment) {
                values
                    .entry(env_name_key(name))
                    .or_insert_with(|| strip_quotes(value.trim()).to_string());
            }
            Some(values)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(HashMap::new()),
        Err(_) => None,
    }
}

/// The names in `home`'s `.env`, as [`env_file_values`] gives them.
pub(super) fn home_env_names(home: &Path) -> Option<BTreeSet<String>> {
    env_file_values(home).map(|values| values.into_keys().collect())
}

impl Keys {
    /// No changes yet; the names in `home`'s `.env` as it stands.
    pub(in crate::chat) fn at_start_up(home: &Path) -> Self {
        let names = home_env_names(home).unwrap_or_default();
        Self {
            changed: Mutex::default(),
            from_file: Mutex::new(names.clone()),
            started_in_home_file: names,
            started_in_project_file: home_env_names(Path::new(PROJECT)).unwrap_or_default(),
        }
    }

    fn names_from_file(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.from_file.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records `names` as seen in the home `.env`.
    pub(super) fn saw_in_home_file(&self, names: impl IntoIterator<Item = String>) {
        self.names_from_file().extend(names);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Option<String>>> {
        // A map updated one entry at a time: a poisoned lock is still safe to use.
        self.changed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this gateway has seen `var` in the home `.env`.
    pub(super) fn came_from_home_file(&self, var: &str) -> bool {
        self.names_from_file().contains(&env_name_key(var))
    }

    /// `var`'s value now, if set and not blank: a change since start-up, else the environment's.
    pub(super) fn value(&self, var: &str) -> Option<String> {
        let value = match self.lock().get(var) {
            Some(changed) => changed.clone(),
            None => std::env::var(var).ok(),
        };
        value.filter(|v| !v.trim().is_empty())
    }

    /// Whether `var` was changed since start-up.
    pub(super) fn changed(&self, var: &str) -> bool {
        self.lock().contains_key(var)
    }

    /// Sets `var` to `value` (`None` removes it); whether its value changed.
    pub(super) fn set(&self, var: &str, value: Option<String>) -> bool {
        let value = value.filter(|v| !v.trim().is_empty());
        let changed = self.value(var) != value;
        self.lock().insert(var.to_string(), value);
        changed
    }

    /// Drops `var`'s value from the home `.env`, whose line is gone, as a fresh start would
    /// read it: the shell's start-up value when the shell set it, else the project `.env`'s
    /// value now (`project`, `None` when it cannot be read; start-up's project value when it
    /// cannot be read or its value would not read back as itself), else removed. A disconnect
    /// stays. Whether its value changed.
    pub(super) fn drop_home_value(
        &self,
        var: &str,
        project: Option<&HashMap<String, String>>,
    ) -> bool {
        if matches!(self.lock().get(var), Some(None)) {
            return false;
        }
        let key = env_name_key(var);
        let in_project = self.started_in_project_file.contains(&key);
        let in_home = self.started_in_home_file.contains(&key);
        if !in_project && !in_home && std::env::var_os(var).is_some() {
            return self.back_to_start_up(var);
        }
        match project.map(|values| values.get(&key)) {
            Some(Some(value)) if env_value_round_trips(var, value) => {
                self.set(var, Some(value.clone()))
            }
            Some(None) => self.set(var, None),
            _ if in_project && !in_home => self.back_to_start_up(var),
            _ => self.set(var, None),
        }
    }

    /// Forgets `var`'s change, so its start-up value applies; whether its value changed.
    fn back_to_start_up(&self, var: &str) -> bool {
        let before = self.value(var);
        self.lock().remove(var);
        self.value(var) != before
    }
}

/// Held across each `.env` change in this process, so 2 key methods at once cannot lose one's
/// change.
pub(super) fn env_write() -> MutexGuard<'static, ()> {
    static WRITE: Mutex<()> = Mutex::new(());
    WRITE.lock().unwrap_or_else(|e| e.into_inner())
}
