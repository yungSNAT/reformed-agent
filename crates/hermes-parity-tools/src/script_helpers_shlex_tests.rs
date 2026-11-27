//! `shlex_split` replaces Python's `shlex.split(command)` (posix mode) in the removed
//! `scripts/run-adapter-chaos-harness.py` and `scripts/run-zero-copy-hotpath-bench.py`.
//! Expected values are Python 3's results for the same input.

use super::shlex_split;

fn split(input: &str) -> Vec<String> {
    shlex_split(input).unwrap()
}

#[test]
fn backslash_newline_outside_quotes_keeps_the_newline() {
    assert_eq!(split("first\\\nsecond"), ["first\nsecond"]);
    assert_eq!(split("\\\nlead"), ["\nlead"]);
    assert_eq!(split("a \\\n b"), ["a", "\n", "b"]);
}

#[test]
fn double_quotes_escape_only_backslash_and_double_quote() {
    assert_eq!(split("tail\"row\\\nname\""), ["tailrow\\\nname"]);
    assert_eq!(split("\"a\\$b\""), ["a\\$b"]);
    assert_eq!(split("\"a\\`b\""), ["a\\`b"]);
    assert_eq!(split("x\"\\\\y\" \"q\\\"z\""), ["x\\y", "q\"z"]);
}

#[test]
fn trailing_backslash_inside_double_quotes_is_an_unfinished_escape() {
    assert_eq!(
        shlex_split("\"weekly\\").unwrap_err(),
        "No escaped character"
    );
}

#[test]
fn only_space_tab_carriage_return_and_newline_separate_words() {
    assert_eq!(split("a\x0cb c"), ["a\x0cb", "c"]);
    assert_eq!(split("a\x0bb"), ["a\x0bb"]);
    assert_eq!(split(" a\tb\r\nc "), ["a", "b", "c"]);
}
