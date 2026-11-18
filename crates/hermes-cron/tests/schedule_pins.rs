//! Next run times pinned from the chrono and `cron` crate implementation, so
//! a change of date library or cron parser keeps every schedule's runs.

use hermes_cron::CronJob;

/// (schedule, after, next run) as the chrono and `cron` implementation gave
/// them on 17 November 2026: one per line, tab-separated, `-` for none.
const NEXT_RUNS: &str = include_str!("schedule_pins.tsv");

/// (schedule, whether `validate` accepts it).
const VALID: &[(&str, bool)] = &[
    ("*/15 * * * *", true),
    ("0 9 * * 1-5", true),
    ("30 2 1 * *", true),
    ("0 0 29 2 *", true),
    ("0 0 31 * *", true),
    ("0 12 * * 0", true),
    ("0 12 * * 7", true),
    ("0 12 * * SUN", true),
    ("0 12 * * sun", true),
    ("0 9 15 * MON", true),
    ("0 9 1-7 * 1", true),
    ("0 9 * * 5-7", true),
    ("0 9 * * 1/2", true),
    ("0 9 * * 0,6", true),
    ("0 0 1 JAN,JUL *", true),
    ("15 14 1 * *", true),
    ("23 0-20/2 * * *", true),
    ("0 0,12 1 */2 *", true),
    ("0 0 1,15 * 3", true),
    ("0 0 ? * MON", true),
    ("0 0 L * *", false),
    ("0 0 * * 1#2", false),
    ("@daily", true),
    ("@hourly", true),
    ("@weekly", true),
    ("@monthly", true),
    ("@yearly", true),
    ("0 30 9 * * *", true),
    ("*/10 * * * * *", true),
    ("0 0 12 ? * MON-FRI", true),
    ("0 0 12 1 * MON", true),
    ("30 0 0 1 1 *", true),
    ("0 0 0 1 1 * 2027", true),
    ("0 0 0 1 1 * 2025", false),
    ("0 0 12 * * * 2026-2027", true),
    ("0 0 30 2 *", false),
    ("0 0 31 2,4 *", false),
    ("61 * * * *", false),
    ("* * *", false),
    ("", false),
    ("abc", false),
    ("* * * * * * * *", false),
];

#[test]
fn next_runs_keep_their_pinned_times() {
    let mut rows = 0;
    for line in NEXT_RUNS.lines() {
        let [schedule, after, next] = line.split('\t').collect::<Vec<_>>()[..] else {
            panic!("malformed pin {line:?}");
        };
        let job = CronJob::new(schedule, "x");
        let expected = (next != "-").then(|| next.parse().unwrap());
        assert_eq!(
            job.compute_next_run(after.parse().unwrap()),
            expected,
            "{schedule:?} after {after}"
        );
        rows += 1;
    }
    assert_eq!(rows, 252);
}

#[test]
fn validation_keeps_its_pinned_verdicts() {
    for (schedule, valid) in VALID {
        assert_eq!(
            CronJob::new(*schedule, "x").validate().is_ok(),
            *valid,
            "{schedule:?}"
        );
    }
}
