//! Evidence for replacing the `cron` crate with `jiff-cron`: both parse the
//! same expressions and give the same runs. Removed with the `cron` crate;
//! `tests/schedule_pins.rs` stays.

use super::CronJob;
use chrono::{DateTime, Utc};
use jiff::tz::TimeZone;
use jiff::Timestamp;

const EXPRESSIONS: &[&str] = &[
    "*/15 * * * *",
    "0 9 * * 1-5",
    "30 2 1 * *",
    "0 0 29 2 *",
    "0 0 31 * *",
    "0 12 * * 0",
    "0 12 * * 7",
    "0 9 13 * 5",
    "0 9 * * 5-7",
    "0 9 * * 1/2",
    "0 9 * * */2",
    "0 9 * * 1-5/2",
    "0 0 1 JAN,JUL *",
    "23 0-20/2 * * *",
    "0 0,12 1 */2 *",
    "0 0 ? * MON",
    "0 0 L * *",
    "0 0 * * 1#2",
    "@daily",
    "@hourly",
    "@weekly",
    "@monthly",
    "@yearly",
    "@annually",
    "@minutely",
    "0 30 9 * * *",
    "*/10 * * * * *",
    "0 0 12 ? * MON-FRI",
    "0 0 12 1 * MON",
    "0 0 9 * * 2",
    "30 0 0 1 1 *",
    "0 0 0 1 1 * 2027",
    "0 0 0 1 1 * 2025",
    "0 0 12 * * * 2026-2027",
    "0 0 0 29 2 * 2028/4",
    "0 0 30 2 *",
    "0 0 31 2,4 *",
    "0 0 0 1-31/7 1-12/3 Sun-Sat/2 *",
    "59 59 23 31 12 * *",
    "0 0 0 * Feb Mon",
    "1,2,3 4-6 7/8 9 10 Tue",
    "61 * * * *",
    "* * *",
    "",
    "abc",
    "* * * * * * * *",
    "0 0 0 0 1 * *",
    "0 0 0 1 13 * *",
    "0 0 0 1 1 8 *",
    "0 0 0 1 1 * 1969",
    "0 0 0 1 1 * 2101",
    "0 0 0 L * *",
    "0 0 0 ? * 5L",
    "0 0 0 ? * 5#3",
    "0 0 0 W * *",
];

struct Sweep(u64);
impl Sweep {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn runs_cron(expression: &str, after: DateTime<Utc>) -> Option<Vec<i128>> {
    let schedule: cron::Schedule = expression.parse().ok()?;
    Some(
        schedule
            .after(&after)
            .take(6)
            .map(|at| {
                i128::from(at.timestamp()) * 1_000_000_000 + i128::from(at.timestamp_subsec_nanos())
            })
            .collect(),
    )
}

fn runs_jiff(expression: &str, after: Timestamp) -> Option<Vec<i128>> {
    let schedule: jiff_cron::Schedule = expression.parse().ok()?;
    Some(
        schedule
            .after(after.to_zoned(TimeZone::UTC))
            .take(6)
            .map(|at| at.timestamp().as_nanosecond())
            .collect(),
    )
}

#[test]
fn jiff_cron_matches_cron_on_raw_and_normalized_expressions() {
    let mut sweep = Sweep(0xD1B5_4A32_D192_ED03);
    let mut instants: Vec<(i64, u32)> = vec![
        (1_794_787_200, 0),           // 2026-11-16T00:00:00Z
        (1_798_761_599, 0),           // 2026-12-31T23:59:59Z
        (1_835_352_000, 500_000_000), // 2028-02-28T12:00:00.5Z
        (4_102_444_740, 0),           // 2099-12-31T23:59:00Z
        (0, 0),
    ];
    for _ in 0..400 {
        // 1990 to 2090, with a fraction half the time.
        let second = 631_152_000 + (sweep.next() % 3_155_760_000) as i64;
        let nanosecond = if sweep.next() % 2 == 0 {
            0
        } else {
            (sweep.next() % 1_000_000_000) as u32
        };
        instants.push((second, nanosecond));
    }
    let mut compared = 0usize;
    for raw in EXPRESSIONS {
        for expression in [raw.to_string(), CronJob::normalize_cron_expr(raw)] {
            let cron_parses = expression.parse::<cron::Schedule>().is_ok();
            let jiff_parses = expression.parse::<jiff_cron::Schedule>().is_ok();
            assert_eq!(cron_parses, jiff_parses, "parse {expression:?}");
            if !cron_parses {
                continue;
            }
            for (second, nanosecond) in &instants {
                let chrono_after = DateTime::from_timestamp(*second, *nanosecond).unwrap();
                let jiff_after = Timestamp::new(*second, *nanosecond as i32).unwrap();
                assert_eq!(
                    runs_cron(&expression, chrono_after),
                    runs_jiff(&expression, jiff_after),
                    "{expression:?} after {chrono_after}"
                );
                compared += 1;
            }
        }
    }
    println!("CRON-EQUIVALENCE compared {compared} (expression, instant) pairs");
}
