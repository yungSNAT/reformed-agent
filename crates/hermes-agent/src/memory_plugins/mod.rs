//! Bundled memory provider plugins.
//!
//! Each module implements `MemoryProviderPlugin` for a specific external
//! memory backend. The built-in provider is always registered first.
//! External providers are additive and can run side-by-side.

pub mod byterover;
mod config_io;
pub(crate) use config_io::default_hermes_home;
pub mod contextlattice;
pub mod hindsight;
pub mod holographic;
pub mod honcho;
pub mod mem0;
pub mod openviking;
pub mod retaindb;
pub mod supermemory;

use std::sync::{Arc, Mutex};

use crate::memory_manager::{MemoryManager, MemoryProviderPlugin};

fn preferred_provider_order() -> Vec<String> {
    std::env::var("HERMES_MEMORY_PROVIDER_ORDER")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|tok| tok.trim().to_ascii_lowercase())
                .filter(|tok| !tok.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            vec![
                "contextlattice".to_string(),
                "honcho".to_string(),
                "supermemory".to_string(),
                "mem0".to_string(),
                "hindsight".to_string(),
                "retaindb".to_string(),
                "openviking".to_string(),
                "byterover".to_string(),
                "holographic".to_string(),
            ]
        })
}

fn sort_by_preferred_order(
    mut providers: Vec<Arc<dyn MemoryProviderPlugin>>,
) -> Vec<Arc<dyn MemoryProviderPlugin>> {
    let order = preferred_provider_order();
    let mut rank = std::collections::HashMap::new();
    for (idx, name) in order.iter().enumerate() {
        rank.insert(name.as_str(), idx);
    }
    providers.sort_by(|a, b| {
        let a_name = a.name().to_ascii_lowercase();
        let b_name = b.name().to_ascii_lowercase();
        let ar = rank.get(a_name.as_str()).copied().unwrap_or(usize::MAX);
        let br = rank.get(b_name.as_str()).copied().unwrap_or(usize::MAX);
        ar.cmp(&br).then_with(|| a_name.cmp(&b_name))
    });
    providers
}

/// Discover and return all available bundled memory providers.
///
/// Checks each provider's `is_available()` without making network calls.
/// Returns them in priority order (API-first, then CLI/local).
pub fn discover_available_providers() -> Vec<Arc<dyn MemoryProviderPlugin>> {
    let mut providers: Vec<Arc<dyn MemoryProviderPlugin>> = Vec::new();

    let honcho = honcho::HonchoMemoryPlugin::new();
    if honcho.is_available() {
        tracing::info!("Discovered memory provider: honcho");
        providers.push(Arc::new(honcho));
    }

    let contextlattice = contextlattice::ContextLatticeMemoryPlugin::new();
    if contextlattice.is_available() {
        tracing::info!("Discovered memory provider: contextlattice");
        providers.push(Arc::new(contextlattice));
    }

    let hindsight = hindsight::HindsightPlugin::new();
    if hindsight.is_available() {
        tracing::info!("Discovered memory provider: hindsight");
        providers.push(Arc::new(hindsight));
    }

    let retaindb = retaindb::RetainDbMemoryPlugin::new();
    if retaindb.is_available() {
        tracing::info!("Discovered memory provider: retaindb");
        providers.push(Arc::new(retaindb));
    }

    let openviking = openviking::OpenVikingMemoryPlugin::new();
    if openviking.is_available() {
        tracing::info!("Discovered memory provider: openviking");
        providers.push(Arc::new(openviking));
    }

    let brv = byterover::ByteRoverPlugin::new();
    if brv.is_available() {
        tracing::info!("Discovered memory provider: byterover");
        providers.push(Arc::new(brv));
    }

    let mem0 = mem0::Mem0MemoryPlugin::new();
    if mem0.is_available() {
        tracing::info!("Discovered memory provider: mem0");
        providers.push(Arc::new(mem0));
    }

    let sm = supermemory::SupermemoryMemoryPlugin::new();
    if sm.is_available() {
        tracing::info!("Discovered memory provider: supermemory");
        providers.push(Arc::new(sm));
    }

    let holo = holographic::HolographicMemoryPlugin::new();
    if holo.is_available() {
        tracing::info!("Discovered memory provider: holographic");
        providers.push(Arc::new(holo));
    }

    sort_by_preferred_order(providers)
}

/// Auto-register the first available external memory provider into the
/// given MemoryManager. Returns the name of the registered provider, if any.
pub fn auto_register_provider(
    manager: &mut crate::memory_manager::MemoryManager,
) -> Option<String> {
    auto_register_providers(manager).into_iter().next()
}

/// Auto-register all available external memory providers into the given
/// MemoryManager. Returns registered provider names in discovery order.
pub fn auto_register_providers(manager: &mut crate::memory_manager::MemoryManager) -> Vec<String> {
    let providers = discover_available_providers();
    let mut names = Vec::new();
    for provider in providers {
        let name = provider.name().to_string();
        manager.add_provider(provider);
        names.push(name);
    }
    names
}

/// Build and initialize a memory manager using all discovered providers.
pub fn build_initialized_memory_manager(
    session_id: &str,
    hermes_home: &str,
    nudge_threshold: u32,
) -> Option<Arc<Mutex<MemoryManager>>> {
    let mut manager = MemoryManager::new().with_nudge_threshold(nudge_threshold.max(1));
    let names = auto_register_providers(&mut manager);
    if names.is_empty() {
        return None;
    }
    for provider in manager.providers() {
        provider.initialize(session_id, hermes_home);
    }
    Some(Arc::new(Mutex::new(manager)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::process::Command;

    fn child_coverage_environment(
        profile: PathBuf,
        temp: Option<OsString>,
    ) -> impl Iterator<Item = (&'static str, PathBuf)> {
        let temp_relative = profile.strip_prefix("%t");
        let temp = temp
            .filter(|path| temp_relative.is_ok() && !path.is_empty())
            .map(|path| std::path::absolute(path).expect("absolute temporary directory"));
        let profile = match temp_relative {
            Ok(_) => profile,
            Err(_) => std::path::absolute(profile).expect("absolute LLVM profile path"),
        };
        std::iter::once(("LLVM_PROFILE_FILE", profile)).chain(temp.map(|path| ("TMPDIR", path)))
    }

    #[test]
    fn child_coverage_environment_preserves_paths_across_directories() {
        let cwd = std::env::current_dir().unwrap();
        let relative = PathBuf::from("coverage/%p.profraw");
        let absolute = cwd.join(&relative);
        let temp_pattern = PathBuf::from("%t/%p.profraw");
        let cases = [
            (relative, Some("tmp".into())),
            (absolute.clone(), Some("tmp".into())),
            (temp_pattern.clone(), Some("tmp".into())),
            (temp_pattern.clone(), None),
            (temp_pattern.clone(), Some(OsString::new())),
        ];
        let environments = cases
            .map(|(profile, temp)| child_coverage_environment(profile, temp).collect::<Vec<_>>());
        let expected = [
            vec![("LLVM_PROFILE_FILE", absolute.clone())],
            vec![("LLVM_PROFILE_FILE", absolute)],
            vec![
                ("LLVM_PROFILE_FILE", temp_pattern.clone()),
                ("TMPDIR", cwd.join("tmp")),
            ],
            vec![("LLVM_PROFILE_FILE", temp_pattern.clone())],
            vec![("LLVM_PROFILE_FILE", temp_pattern)],
        ];
        assert_eq!(environments, expected);
    }

    #[test]
    fn contextlattice_is_discovered_initialized_and_ordered() {
        const CASE: &str = "HERMES_TEST_CONTEXTLATTICE_AUTO_REGISTRATION";
        if let Ok(case) = std::env::var(CASE) {
            let home = std::env::var("HERMES_HOME").unwrap();
            let manager = build_initialized_memory_manager("carry-fixture", &home, 1).unwrap();
            let manager = manager.lock().unwrap();
            let providers: Vec<&str> = manager
                .providers()
                .iter()
                .map(|provider| provider.name())
                .filter(|name| matches!(*name, "contextlattice" | "holographic"))
                .collect();
            match case.as_str() {
                "disabled" => assert_eq!(providers, ["holographic"]),
                "enabled" => assert_eq!(providers, ["contextlattice", "holographic"]),
                "custom-order" => assert_eq!(providers, ["holographic", "contextlattice"]),
                _ => panic!("unknown fixture case {case}"),
            }
            if case != "disabled" {
                let provider = manager.get_provider("contextlattice").unwrap();
                assert!(provider
                    .system_prompt_block()
                    .contains("Project: carry-fixture."));
            }
            return;
        }

        // Keep provider discovery away from the developer's configuration and other tests.
        let executable = std::env::current_exe().unwrap();
        let (_, module) = module_path!().split_once("::").unwrap();
        let test = format!("{module}::contextlattice_is_discovered_initialized_and_ordered");
        for case in ["disabled", "enabled", "custom-order"] {
            let home = tempfile::tempdir().unwrap();
            let mut command = Command::new(&executable);
            command
                .env_clear()
                .env(CASE, case)
                .env("HOME", home.path())
                .env("USERPROFILE", home.path())
                .env("HERMES_HOME", home.path())
                .env("PATH", home.path())
                .current_dir(home.path())
                .envs(
                    std::env::var_os("LLVM_PROFILE_FILE")
                        .into_iter()
                        .flat_map(|profile| {
                            child_coverage_environment(
                                PathBuf::from(profile),
                                std::env::var_os("TMPDIR"),
                            )
                        }),
                )
                .args(["--exact", &test]);
            #[cfg(windows)]
            if let Some(system_root) = std::env::var_os("SystemRoot") {
                command.env("SystemRoot", system_root);
            }
            if case != "disabled" {
                command
                    .env("HERMES_ENABLE_CONTEXTLATTICE", "true")
                    .env("CONTEXTLATTICE_PROJECT", "carry-fixture");
            }
            if case == "custom-order" {
                command.env("HERMES_MEMORY_PROVIDER_ORDER", "holographic,contextlattice");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let passed = format!("test {test} ... ok");
            assert!(
                output.status.success() && stdout.lines().any(|line| line == passed),
                "{case}: {output:?}"
            );
        }
    }
}
