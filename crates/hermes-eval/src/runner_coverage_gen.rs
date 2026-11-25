use super::*;
use crate::adapter::BenchmarkMetadata;
use crate::verifier::Verifier;
use serde::de::value::{Error as FixtureError, MapDeserializer, SeqDeserializer};
use serde::de::{IntoDeserializer, Visitor};
use serde_json::{json, Value};
use std::time::Duration;

// Give adapter-specific fields neutral values. The tests set the three
// TaskSpec fields the runner reads explicitly in fixture_task.
#[derive(Clone, Copy)]
struct FixtureValue;

impl<'de> IntoDeserializer<'de, FixtureError> for FixtureValue {
    type Deserializer = Self;

    fn into_deserializer(self) -> Self::Deserializer {
        self
    }
}

macro_rules! fixture_scalars {
    ($($method:ident => $visit:ident($value:expr)),+ $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                visitor.$visit($value)
            }
        )+
    };
}

impl<'de> serde::Deserializer<'de> for FixtureValue {
    type Error = FixtureError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fixture_scalars! {
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
        deserialize_string => visit_borrowed_str(""),
        deserialize_bytes => visit_borrowed_bytes(b""),
        deserialize_byte_buf => visit_borrowed_bytes(b""),
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_none()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(0, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(SeqDeserializer::new((0..len).map(|_| self)))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_map(MapDeserializer::new(std::iter::empty::<(&str, Self)>()))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let variant = variants
            .first()
            .copied()
            .ok_or_else(|| serde::de::Error::custom("fixture enum has no variants"))?;
        visitor.visit_enum(variant.into_deserializer())
    }

    serde::forward_to_deserialize_any! {
        unit unit_struct identifier ignored_any
    }
}

fn fixture_task(id: &str, category: Option<&str>) -> TaskSpec {
    let mut task = TaskSpec::deserialize(FixtureValue).expect("construct task fixture");
    task.task_id = id.into();
    task.category = category.map(str::to_owned);
    task.timeout = if id == "timeout" {
        Duration::ZERO
    } else {
        Duration::from_secs(5)
    };
    task
}

struct FixtureRollout;

#[async_trait]
impl TaskRollout for FixtureRollout {
    async fn execute(&self, task: &TaskSpec) -> EvalResult<Value> {
        match task.task_id.as_str() {
            "timeout" => std::future::pending().await,
            "rollout-error" => Err(EvalError::TaskExecution("fixture rollout error".into())),
            _ => Ok(json!({ "task_id": task.task_id })),
        }
    }
}

struct FixtureVerifier;

#[async_trait]
impl Verifier for FixtureVerifier {
    async fn verify(&self, task: &TaskSpec, state: &Value) -> EvalResult<VerificationOutcome> {
        assert!(!matches!(
            task.task_id.as_str(),
            "timeout" | "rollout-error"
        ));
        assert_eq!(state, &json!({ "task_id": task.task_id }));
        if task.task_id == "verification-error" {
            Err(EvalError::TaskExecution(
                "fixture verification error".into(),
            ))
        } else {
            Ok(VerificationOutcome::pass())
        }
    }
}

struct FixtureAdapter {
    ids: &'static [&'static str],
}

#[async_trait]
impl BenchmarkAdapter for FixtureAdapter {
    fn metadata(&self) -> BenchmarkMetadata {
        BenchmarkMetadata {
            id: "coverage".into(),
            name: "Coverage fixture".into(),
            source: "in-memory".into(),
            version: "1".into(),
        }
    }

    async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
        Ok(self.ids.iter().map(|id| fixture_task(id, None)).collect())
    }

    fn verifier(&self) -> Box<dyn Verifier> {
        Box::new(FixtureVerifier)
    }

    async fn post_process(
        &self,
        task: &TaskSpec,
        outcome: VerificationOutcome,
    ) -> EvalResult<VerificationOutcome> {
        assert!(outcome.passed);
        match task.task_id.as_str() {
            "verification-error" => panic!("post-processing ran after a verification error"),
            "post-process-error" => Err(EvalError::TaskExecution(
                "fixture post-process error".into(),
            )),
            "failed" => Ok(VerificationOutcome::fail("fixture rejected the result")),
            _ => Ok(outcome),
        }
    }
}

#[tokio::test]
async fn task_errors_identify_the_stage_and_preserve_the_completed_verdict() {
    let execution_error = |message: &str| EvalError::TaskExecution(message.into()).to_string();
    let cases = [
        (
            "timeout",
            TaskStatus::Timeout,
            "timeout after 0ns".into(),
            false,
        ),
        (
            "rollout-error",
            TaskStatus::Error,
            execution_error("fixture rollout error"),
            false,
        ),
        (
            "verification-error",
            TaskStatus::Error,
            execution_error("fixture verification error"),
            false,
        ),
        (
            "post-process-error",
            TaskStatus::Error,
            execution_error("fixture post-process error"),
            true,
        ),
    ];

    for (id, status, message, verifier_passed) in cases {
        let result = run_one_task(
            Arc::new(FixtureAdapter { ids: &[] }),
            Arc::new(FixtureRollout),
            fixture_task(id, Some("fixture category")),
        )
        .await;

        assert_eq!(result.task_id, id);
        assert_eq!(result.category.as_deref(), Some("fixture category"));
        assert_eq!(
            std::mem::discriminant(&result.status),
            std::mem::discriminant(&status),
            "wrong status for {id}"
        );
        assert_eq!(result.error.as_deref(), Some(message.as_str()));
        assert_eq!(
            result.outcome.passed, verifier_passed,
            "wrong verdict for {id}"
        );
        assert_eq!(result.agent_turns, 0);
        assert_eq!(result.tokens_input, 0);
        assert_eq!(result.tokens_output, 0);
        assert!(result.cost_usd.abs() < f64::EPSILON);
    }
}

#[tokio::test]
async fn strict_run_reports_the_first_bad_task_in_dataset_order() {
    let adapter = Arc::new(FixtureAdapter {
        ids: &["passed", "failed", "rollout-error"],
    });
    let rollout = Arc::new(FixtureRollout);
    let mut runner = Runner::new(RunnerConfig {
        concurrency: 2,
        continue_on_error: false,
        ..RunnerConfig::default()
    });

    let error = runner
        .run(Arc::clone(&adapter), Arc::clone(&rollout))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EvalError::TaskExecution(ref message) if message == "task failed status Failed: None"
    ));

    runner.config.continue_on_error = true;
    let record = runner.run(adapter, rollout).await.unwrap();
    let ids: Vec<_> = record
        .tasks
        .iter()
        .map(|task| task.task_id.as_str())
        .collect();
    assert_eq!(ids, ["passed", "failed", "rollout-error"]);
    assert!(matches!(record.tasks[0].status, TaskStatus::Passed));
    assert!(matches!(record.tasks[1].status, TaskStatus::Failed));
    assert!(!record.tasks[1].outcome.passed);
    assert_eq!(record.tasks[1].agent_turns, 1);
    assert!(record.tasks[1].error.is_none());
    assert!(matches!(record.tasks[2].status, TaskStatus::Error));
    assert_eq!(record.metrics.passed, 1);
    assert_eq!(record.metrics.failed, 1);
    assert_eq!(record.metrics.error, 1);
}

#[test]
fn filters_match_ids_or_categories_before_applying_the_cap() {
    let mut tasks = vec![
        fixture_task("unrelated", None),
        fixture_task("other-task", Some("other category")),
        fixture_task("ID-Needle", None),
        fixture_task("category-match", Some("Terminal Tasks")),
        fixture_task("another-needle", Some("other")),
    ];

    apply_task_filters(&mut tasks, Some(2), Some(" , NEEDLE , tErMiNaL, "));
    let ids: Vec<_> = tasks.iter().map(|task| task.task_id.as_str()).collect();
    assert_eq!(ids, ["ID-Needle", "category-match"]);

    apply_task_filters(&mut tasks, None, Some(" , "));
    assert_eq!(tasks.len(), 2);

    apply_task_filters(&mut tasks, Some(0), None);
    assert!(tasks.is_empty());
}

#[test]
fn aggregation_counts_all_statuses_and_accumulates_usage() {
    let tasks: Vec<_> = [
        TaskStatus::Passed,
        TaskStatus::Failed,
        TaskStatus::Timeout,
        TaskStatus::Error,
        TaskStatus::Skipped,
    ]
    .into_iter()
    .zip(1_u64..)
    .map(|(status, n)| TaskResult {
        task_id: n.to_string(),
        category: None,
        outcome: if matches!(status, TaskStatus::Passed) {
            VerificationOutcome::pass()
        } else {
            VerificationOutcome::fail("fixture did not pass")
        },
        status,
        agent_turns: 0,
        duration: Duration::from_millis(n),
        tokens_input: n * 2,
        tokens_output: n * 3,
        cost_usd: n as f64 * 0.25,
        error: None,
    })
    .collect();

    let metrics = aggregate_metrics(&tasks);
    assert_eq!(metrics.total, 5);
    assert_eq!(metrics.passed, 1);
    assert_eq!(metrics.failed, 1);
    assert_eq!(metrics.timeout, 1);
    assert_eq!(metrics.error, 1);
    assert_eq!(metrics.skipped, 1);
    assert!((metrics.pass_at_1 - 0.2).abs() < f64::EPSILON);
    assert_eq!(metrics.total_duration, Duration::from_millis(15));
    assert_eq!(metrics.total_tokens_input, 30);
    assert_eq!(metrics.total_tokens_output, 45);
    assert!((metrics.total_cost_usd - 3.75).abs() < f64::EPSILON);
}

#[test]
fn empty_aggregation_has_zero_pass_rate_and_usage() {
    let metrics = aggregate_metrics(&[]);
    assert_eq!(metrics.total, 0);
    assert!(metrics.pass_at_1.abs() < f64::EPSILON);
    assert_eq!(metrics.total_duration, Duration::ZERO);
    assert_eq!(metrics.total_tokens_input, 0);
    assert_eq!(metrics.total_tokens_output, 0);
    assert!(metrics.total_cost_usd.abs() < f64::EPSILON);
}
