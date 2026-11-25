use super::*;

use serde::de::{
    self,
    value::{Error as FixtureError, MapDeserializer, SeqDeserializer, StrDeserializer},
    IntoDeserializer, Visitor,
};
use std::ffi::OsStr;
use std::process::Command;

fn in_isolated_runtime(test_name: &str, check: impl FnOnce()) {
    const CHILD_TEST: &str = "HERMES_POLICIES_COVERAGE_CHILD_TEST";

    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::{test_name}");

    if std::env::var_os(CHILD_TEST).as_deref() == Some(OsStr::new(&test_name)) {
        let home = std::env::var_os("HERMES_HOME").expect("isolated Hermes home");
        assert!(objective_simulation_policy_path().starts_with(&home));
        check();
        return;
    }

    // Isolate path resolution without changing the environment of parallel tests.
    let sandbox = tempfile::tempdir().expect("temporary runtime directory");
    let mut child = Command::new(std::env::current_exe().expect("test executable")); // nosemgrep: rust.lang.security.current-exe.current-exe
    child
        .env_clear()
        .current_dir(sandbox.path())
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(CHILD_TEST, &test_name);

    for key in [
        "HERMES_HOME",
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        child.env(key, sandbox.path());
    }

    for key in ["PATH", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }

    if let Some(pattern) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir()
                .expect("test working directory")
                .join(pattern),
        );
    }

    let output = child.output().expect("run isolated policy test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{test_name}: {}\n{stdout}\n{stderr}",
        output.status
    );
}

// Supply type-appropriate values for fields that do not affect the behavior under test.
struct Fixture;

impl<'de> IntoDeserializer<'de, FixtureError> for Fixture {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self {
        self
    }
}

macro_rules! fixture_scalars {
    ($lt:lifetime; $($method:ident => $visit:ident($($value:expr)?)),+ $(,)?) => {
        $(
            fn $method<V: Visitor<$lt>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                visitor.$visit($($value)?)
            }
        )+
    };
}

impl<'de> de::Deserializer<'de> for Fixture {
    type Error = FixtureError;

    fixture_scalars! {
        'de;
        deserialize_any => visit_unit(),
        deserialize_bool => visit_bool(false),
        deserialize_i8 => visit_i8(0),
        deserialize_i16 => visit_i16(0),
        deserialize_i32 => visit_i32(0),
        deserialize_i64 => visit_i64(0),
        deserialize_i128 => visit_i128(0),
        deserialize_u8 => visit_u8(0),
        deserialize_u16 => visit_u16(0),
        deserialize_u32 => visit_u32(0),
        deserialize_u64 => visit_u64(0),
        deserialize_u128 => visit_u128(0),
        deserialize_f32 => visit_f32(0.0),
        deserialize_f64 => visit_f64(0.0),
        deserialize_char => visit_char('x'),
        deserialize_str => visit_borrowed_str(""),
        deserialize_string => visit_string(String::new()),
        deserialize_bytes => visit_borrowed_bytes(&[]),
        deserialize_byte_buf => visit_byte_buf(Vec::new()),
        deserialize_option => visit_none(),
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_map(MapDeserializer::new(
            fields.iter().copied().map(|field| (field, Fixture)),
        ))
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_map(MapDeserializer::new(std::iter::empty::<(&str, Fixture)>()))
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(SeqDeserializer::new(std::iter::empty::<Fixture>()))
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(SeqDeserializer::new((0..len).map(|_| Fixture)))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let variant = variants
            .first()
            .copied()
            .ok_or_else(|| <FixtureError as de::Error>::custom("fixture enum has no variants"))?;
        visitor.visit_enum(StrDeserializer::<FixtureError>::new(variant))
    }

    serde::forward_to_deserialize_any! {
        unit identifier ignored_any
    }
}

fn fixture<T: de::DeserializeOwned>() -> T {
    T::deserialize(Fixture).expect("deserialize policy test fixture")
}

fn fixture_for_optional_loader<T: de::DeserializeOwned>(
    _loader: fn() -> Result<Option<T>, AgentError>,
) -> T {
    fixture()
}

#[test]
fn simulation_and_ensemble_loaders_read_their_policy_files() {
    in_isolated_runtime(
        "simulation_and_ensemble_loaders_read_their_policy_files",
        || {
            let simulation = set_objective_simulation_mode("shadow").unwrap();
            let ensemble = set_objective_ensemble_mode("off").unwrap();

            assert_eq!(
                serde_json::to_value(load_objective_simulation_policy().unwrap()).unwrap(),
                serde_json::to_value(&simulation).unwrap()
            );
            assert_eq!(
                serde_json::to_value(load_objective_ensemble_policy().unwrap()).unwrap(),
                serde_json::to_value(ensemble).unwrap()
            );

            let simulation_path = objective_simulation_policy_path();
            std::fs::write(&simulation_path, b"{").unwrap();
            assert!(load_objective_simulation_policy().is_err());
            write_json_file(&simulation_path, &simulation).unwrap();

            std::fs::write(objective_ensemble_policy_path(), b"{").unwrap();
            assert!(load_objective_ensemble_policy().is_err());
        },
    );
}

fn old_ledger_timestamp(index: usize) -> String {
    format!("2000-01-01T00:{:02}:{:02}Z", index / 60, index % 60)
}

#[test]
fn learning_ledger_discards_oldest_entry_and_can_be_cleared() {
    in_isolated_runtime(
        "learning_ledger_discards_oldest_entry_and_can_be_cleared",
        || {
            let mut ledger = load_objective_learning_ledger().unwrap();
            ledger.entries = (0..512)
                .map(|index| {
                    let mut entry = fixture::<ObjectiveLearningLedgerEntry>();
                    entry.recorded_at = old_ledger_timestamp(index);
                    entry
                })
                .collect();
            write_json_file(&objective_learning_ledger_path(), &ledger).unwrap();

            let mut entry = fixture::<ObjectiveLearningLedgerEntry>();
            entry.recorded_at = " \t ".to_owned();
            let updated = append_objective_learning_entry(entry).unwrap();
            assert_eq!(updated.entries.len(), 512);
            for (index, retained) in updated.entries[..511].iter().enumerate() {
                assert_eq!(retained.recorded_at, old_ledger_timestamp(index + 1));
            }
            assert!(updated.entries[511]
                .recorded_at
                .parse::<jiff::Timestamp>()
                .is_ok());
            assert_eq!(
                serde_json::to_value(load_objective_learning_ledger().unwrap()).unwrap(),
                serde_json::to_value(updated).unwrap()
            );

            let cleared = clear_objective_learning_ledger().unwrap();
            assert!(cleared.entries.is_empty());
            assert_eq!(
                serde_json::to_value(load_objective_learning_ledger().unwrap()).unwrap(),
                serde_json::to_value(cleared).unwrap()
            );
        },
    );
}

#[test]
fn dag_requires_a_contract_and_gates_trading_work_on_shadow_verification() {
    in_isolated_runtime(
        "dag_requires_a_contract_and_gates_trading_work_on_shadow_verification",
        || {
            match build_objective_dag_from_contract() {
                Err(AgentError::Config(message)) => assert_eq!(
                    message,
                    "objective contract is not initialized; run `/objective <text>` first"
                ),
                _ => panic!("building a DAG without a contract must return a config error"),
            }

            let mut contract = fixture_for_optional_loader(load_objective_contract);
            contract.id = "coverage-objective".to_owned();
            contract.trading_sensitive = true;
            write_json_file(&objective_contract_path(), &contract).unwrap();

            let dag = build_objective_dag_from_contract().unwrap();
            assert_eq!(dag.objective_id, contract.id);
            assert_eq!(dag.auto_resume_checkpoint, "discover");
            assert_eq!(
                dag.nodes
                    .iter()
                    .map(|node| node.id.as_str())
                    .collect::<Vec<_>>(),
                ["discover", "design", "implement", "verify", "shadow"]
            );
            let shadow = dag.nodes.last().unwrap();
            assert_eq!(shadow.title, "Shadow/simulator gate before promotion");
            assert_eq!(shadow.status, "pending");
            assert_eq!(shadow.depends_on.as_slice(), &["verify"]);
            assert_eq!(shadow.rollback, "reduce_exposure_and_rerun");
            assert_eq!(
                serde_json::to_value(load_objective_dag().unwrap()).unwrap(),
                serde_json::to_value(dag).unwrap()
            );
        },
    );
}

#[test]
fn eval_trend_retains_the_latest_512_samples() {
    in_isolated_runtime("eval_trend_retains_the_latest_512_samples", || {
        let mut trend = load_objective_eval_trend().unwrap();
        trend.samples = (0..512)
            .map(|index| ObjectiveEvalSample {
                recorded_at: old_ledger_timestamp(index),
                objective_id: format!("existing-{index}"),
                objective_state: "pending".to_owned(),
                score: score_for_objective_state("pending"),
                note: String::new(),
            })
            .collect();
        write_json_file(&objective_eval_trend_path(), &trend).unwrap();

        let updated =
            append_objective_eval_sample(" new-objective ", " verified ", " checked ").unwrap();
        assert_eq!(updated.samples.len(), 512);
        for (index, sample) in updated.samples[..511].iter().enumerate() {
            assert_eq!(sample.objective_id, format!("existing-{}", index + 1));
        }
        let newest = &updated.samples[511];
        assert_eq!(newest.objective_id, "new-objective");
        assert_eq!(newest.objective_state, "verified");
        assert_eq!(newest.note, "checked");
        assert_eq!(
            serde_json::to_value(load_objective_eval_trend().unwrap()).unwrap(),
            serde_json::to_value(updated).unwrap()
        );
    });
}

#[test]
fn unrecognized_subagent_role_gets_persisted_default_budget_floors() {
    in_isolated_runtime(
        "unrecognized_subagent_role_gets_persisted_default_budget_floors",
        || {
            let path = subagent_registry_path();
            let mut registry = load_subagent_registry().unwrap();
            if registry.profiles.is_empty() {
                registry.profiles.push(fixture());
            }
            let profile = &mut registry.profiles[0];
            profile.role = " Coverage Custom Role ".to_owned();
            profile.budget.max_turns = 1;
            profile.budget.max_tool_calls = 2;
            profile.budget.max_tokens = 3;
            let old_timestamp = old_ledger_timestamp(0);
            registry.updated_at.clone_from(&old_timestamp);
            write_json_file(&path, &registry).unwrap();

            let repaired = load_subagent_registry().unwrap();
            let profile = &repaired.profiles[0];
            assert_eq!(profile.role, " Coverage Custom Role ");
            assert_eq!(profile.budget.max_turns, 48);
            assert_eq!(profile.budget.max_tool_calls, 120);
            assert_eq!(profile.budget.max_tokens, 180_000);
            assert_ne!(repaired.updated_at, old_timestamp);

            let mut persisted: SubagentRegistry = read_json_file(&path).unwrap();
            assert_eq!(
                serde_json::to_value(&persisted).unwrap(),
                serde_json::to_value(repaired).unwrap()
            );
            persisted.updated_at.clone_from(&old_timestamp);
            write_json_file(&path, &persisted).unwrap();
            assert_eq!(load_subagent_registry().unwrap().updated_at, old_timestamp);
        },
    );
}

#[test]
fn contextlattice_fills_empty_fields_and_normalizes_retrieval_mode() {
    in_isolated_runtime(
        "contextlattice_fills_empty_fields_and_normalizes_retrieval_mode",
        || {
            let defaults = ContextLatticePolicy::default();
            let mut input = load_contextlattice_policy().unwrap();
            input.checkpoint_write_policy.clear();
            input.shared_topic_taxonomy.clear();
            input.deep_retry_budget_secs.clear();
            input.regular_retry_budget_secs.clear();
            input.summary_sink_order.clear();
            input.preferred_retrieval_mode = " \t ".to_owned();

            let restored = set_contextlattice_policy(input).unwrap();
            assert_eq!(
                restored.checkpoint_write_policy,
                defaults.checkpoint_write_policy
            );
            assert_eq!(
                restored.shared_topic_taxonomy,
                defaults.shared_topic_taxonomy
            );
            assert_eq!(
                restored.deep_retry_budget_secs,
                defaults.deep_retry_budget_secs
            );
            assert_eq!(
                restored.regular_retry_budget_secs,
                defaults.regular_retry_budget_secs
            );
            assert_eq!(restored.summary_sink_order, defaults.summary_sink_order);
            assert_eq!(
                restored.preferred_retrieval_mode,
                defaults.preferred_retrieval_mode
            );
            assert_eq!(
                serde_json::to_value(load_contextlattice_policy().unwrap()).unwrap(),
                serde_json::to_value(restored).unwrap()
            );

            for (mode, expected) in [(" FaSt ", "fast"), (" unrecognized-mode ", "deep")] {
                let mut input = load_contextlattice_policy().unwrap();
                input.preferred_retrieval_mode = mode.to_owned();
                assert_eq!(
                    set_contextlattice_policy(input)
                        .unwrap()
                        .preferred_retrieval_mode,
                    expected
                );
                assert_eq!(
                    load_contextlattice_policy()
                        .unwrap()
                        .preferred_retrieval_mode,
                    expected
                );
            }
        },
    );
}
