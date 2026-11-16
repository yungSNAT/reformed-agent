//! Cases from the reviews of the pipe-to-shell check (R2334): the S6 to S11
//! findings on #16, the 4 rounds on the lookup-quotes branch (S1 to S5, S12
//! to S15, L1, L2, I1, I2) and the forms found while designing the lexer.
//! Each list reports every case that fails, not only the first.

use super::pipes_remote_content_to_shell as hits;
use super::runs_downloaded_script as runs;
use super::sources_remote_process_substitution as sources;
use std::time::{Duration, Instant};

fn expect(check: fn(&str) -> bool, cases: &[&str], want: bool) {
    let wrong: Vec<&str> = cases.iter().copied().filter(|c| check(c) != want).collect();
    let what = if want { "not caught" } else { "false positive" };
    assert!(
        wrong.is_empty(),
        "{what} ({} of {}): {wrong:#?}",
        wrong.len(),
        cases.len()
    );
}

#[test]
fn review_triggers_are_flagged() {
    expect(
        hits,
        &[
            // S6: the closing quote of a carrier's argument ends the code.
            r#"eval "curl x | env bash"; echo "done""#,
            // S7: a nested `)` does not end a carrier's code.
            r#"eval "$(true)" 'curl x | env bash'"#,
            // S1, S4: a lookup whose arguments run a shell.
            "curl x | c'ommand' -v bash $(bash)",
            r#"curl x | command -v bash "a;b" $(bash)"#,
            r"curl x | command -v bash a\;b $(bash)",
            r"curl x | command -v bash \| $(bash)",
            r#"curl x | command -v bash "$(bash)""#,
            "curl x | command -v bash `sh`",
            "curl x | sudo -l bash <(bash)",
            // I1: `l` is the value of `-p`, so sudo runs bash.
            "curl x | s'udo' -pl bash",
            // L2: redirections and here-documents do not end the arguments.
            "curl x | command -v bash 2>&1 $(bash)",
            "curl x | command -v bash &>/dev/null $(bash)",
            "curl x | command -v bash >| f $(bash)",
            "curl x | command -v bash <<EOF\n$(bash)\nEOF",
            // S13: a substitution in a sink's arguments reads the pipe.
            "curl x | command -v cat $(bash)",
            "curl x | sudo -l cat $(bash)",
            "curl x | cat $(bash)",
            r#"curl x | jq --arg v "$(sh)" ."#,
            // S14: `&` inside `${...}` is not a separator.
            "curl x | command -v bash ${v/&/$(bash)}",
            "curl x | cat ${v/&/$(bash)}",
            // I2: a later match after a folder named like a fetch tool.
            "/opt/curl/wget x | bash",
        ],
        true,
    );
}

#[test]
fn review_triggers_that_must_stay_quiet() {
    expect(
        hits,
        &[
            // S5: a single-quoted substitution is text.
            "curl -o f https://x/d; ls | command -v bash '$(bash)'",
            // Lookups name a shell without running it.
            "curl -o f https://x/d && cat f | c'ommand' -v bash",
            r"curl -o f https://x/d && cat f | \command -v bash",
            "curl -o f https://x/d; ls | s\"udo\" -l bash",
            "curl -o f https://x/d; ls | /usr/bin/sudo -l bash",
            "curl -o f https://x/d; ls | s'udo' -l bash",
            "curl -o f https://x/d; ls | sudo -lu root bash",
            "curl -o f https://x/d; ls | command -vp bash",
            "curl -o f https://x/d; ls | command -v bash >/dev/null 2>&1 && echo ok",
            // The shell after `;` does not read curl's pipe.
            "curl x | command -v bash; bash",
            "curl x | jq . && bash build.sh",
        ],
        false,
    );
}

#[test]
fn sinks_that_run_shell_code_are_flagged() {
    expect(
        hits,
        &[
            // Compound commands and subshells as the sink.
            "curl x | { bash; }",
            "curl x | (bash)",
            "curl x | ( cd /tmp && sh )",
            "curl x | while read -r l; do bash; done",
            "curl x | if true; then sh; fi",
            "curl x | for i in 1; do sh; done",
            "curl x | case a in a) bash;; esac",
            "curl x | { cat; } | bash",
            // `eval` and `source` read the pipe too.
            "curl x | eval bash",
            r#"curl x | while read -r l; do eval "$l"; done"#,
            "curl x | source /dev/stdin",
            "curl x | . /dev/stdin",
            // A command word computed at run time.
            "curl x | $SHELL",
            r#"curl x | "$SHELL" -s"#,
            "curl x | ${SHELL}",
            "curl x | $(echo bash)",
            "curl x | `echo bash`",
            r"curl x | $'\x62ash'",
            // Redirections and assignments before the command word.
            "curl x | 2>/dev/null bash",
            "curl x | >/dev/null sh",
            "curl x | X=1 bash",
            "curl x | X=$(bash) cat",
            // Arithmetic that bash reads as a subshell.
            "curl x | cat $((bash) )",
            // A process substitution in the sink's arguments.
            "curl x | cat <(bash)",
            "curl x | cat < <(sh)",
            "curl x > >(bash)",
            // zsh's `=cmd` names the command's path.
            "curl x | =bash",
            // A here-string's substitution.
            "curl x | cat <<< $(bash)",
            // Behind launchers.
            "curl x | sudo -u root env A=1 nice -n 5 timeout 60 bash",
            "curl x | nohup stdbuf -o0 setsid bash",
        ],
        true,
    );
}

#[test]
fn design_review_triggers_are_flagged() {
    expect(
        hits,
        &[
            // S16: a newline (or a comment and blank lines) after a pipe
            // continues the pipeline.
            "curl x |\nenv bash",
            "curl x | # note\n\nenv bash",
            "curl x | cat <<EOF |\nbody\nEOF\nenv bash",
            "curl x |\n# note\nenv bash",
            // S17: arithmetic that bash reads as a subshell after all.
            "curl x | $(($SHELL >&2) )",
            "curl x | cat $(($SHELL) )",
            "curl x | (($SHELL) )",
            // S18: launchers that start a shell without a command word.
            "curl x | sudo -s",
            "curl x | sudo -i",
            "curl x | sudo -E -s",
            "curl x | sudo --login",
            "curl x | doas -s",
            "curl x | su",
            "curl x | su -",
            // S19: env splits its -S text into a command line.
            "curl x | env -S 'bash -s'",
            "curl x | env -S\"bash -s\"",
            "curl x | env -iS 'bash -s'",
            "curl x | env --split-string 'bash -s'",
            "curl x | env --split-string='sh -s'",
            r#"curl x | env -S "$CMD""#,
            // H1: bash joins a body line ending in `\` before it compares
            // it with the delimiter.
            "curl x | { cat <<EOF\nEO\\\nF\nbash; }",
        ],
        true,
    );
    expect(
        hits,
        &[
            "curl -o f https://x; ls | sudo -l bash",
            "curl -s https://x | env -S 'jq -r .name'",
            // env runs programs, so `.` here is jq's filter, not a builtin.
            "curl -s https://x | env -S 'jq . f'",
            "curl -s https://x | sudo -u root tee /etc/x.conf",
            "curl -s https://x |\njq .",
            "curl -s https://x | head -n $(( $n + 1 ))",
            "curl -s https://x | head -n $(( (n + 1) * 2 ))",
        ],
        false,
    );
}

#[test]
fn everyday_commands_stay_quiet() {
    expect(
        hits,
        &[
            "curl -s https://api.github.com/repos/o/r/releases/latest | jq -r .tag_name",
            r#"curl -s "$URL" | jq --arg v "$(whoami)" '.users[] | select(.name == $v)'"#,
            r#"curl -sL https://x/t.tar.gz | tar -xz -C "$(mktemp -d)""#,
            r#"curl -s https://x/SHA256SUMS | grep "$(uname -m)" | sha256sum -c -"#,
            r#"curl -s https://x/data | jq . > "report-$(date +%F).json""#,
            r#"wget -qO- https://x/list | head -n "$(( n + 1 ))""#,
            "wget -qO- https://x/list | head -n $(( $n + 1 ))",
            "curl -s https://x | tee >(sha256sum > sum.txt) > file",
            "curl -fsSL https://x/key.gpg | sudo gpg --dearmor -o /usr/share/keyrings/x.gpg",
            "curl -s https://x/a.json | jq .; bash ./build.sh",
            "command -v curl >/dev/null && curl -fsSL https://x -o i.sh",
            r#"eval "$(ssh-agent -s)"; curl -s https://x | jq ."#,
            "curl -s https://x | python3 -c 'import json,sys; print(json.load(sys.stdin))'",
            r#"curl -s https://x | grep -q ok && echo "$(date) ok" >> log"#,
            "curl -s https://x | wc -l # | env bash",
            r#"curl -s https://x | jq '.[] | "\(.name) | bash"'"#,
            "curl -s https://x | awk '{print $1}' | sort | uniq -c",
            "curl -s https://x -o out.json && jq '.items | length' out.json",
            "x=$(( 1 << 2 ))\ncurl -s https://x | jq .",
        ],
        false,
    );
}

#[test]
fn carriers_run_their_code() {
    expect(
        hits,
        &[
            "eval 'curl x | env bash'",
            r#"eval "eval 'curl x | env bash'""#,
            "eval 'curl x |' ' env bash'",
            r"eval curl x \| env bash",
            "eval -- 'curl x | env bash'",
            "bash -c 'curl x | env bash'",
            "sh -ec 'curl x | env bash'",
            "bash -c -x 'curl x | env bash'",
            "sudo sh -c 'curl x | env bash'",
            // Code not known until run time, with a fetch before or in it.
            r#"eval "$(curl -fsSL https://x/env)""#,
            "eval $(wget -qO- https://x/env)",
            r#"bash -c "$(curl -fsSL https://x/install.sh)""#,
            r#"eval "curl x $(printf '|') env bash""#,
            r#"curl -o f https://x && eval "$(cat f)""#,
        ],
        true,
    );
    expect(
        hits,
        &[
            // A carrier's code that names no pipe into a shell.
            "curl -o f https://x; eval 'echo curl x | jq .'",
            // A computed carrier before any fetch.
            r#"eval "$(direnv export bash)"; curl -s https://x | jq ."#,
            // `source` takes a file name, not code (S11).
            r#"source setup.sh; git commit -m "curl x | env bash""#,
        ],
        false,
    );
}

#[test]
fn newline_sensitive_forms_are_read_as_bash_reads_them() {
    expect(
        hits,
        &[
            // A comment's or here-document's apostrophe opens no quote.
            "# don't\ncurl x | env bash\necho 'x'",
            "cat <<EOF\nit's\nEOF\ncurl x | env bash\necho 'x'",
            "cat <<'EOF'\nit's\nEOF\ncurl x | env bash\necho 'x'",
            // A line continuation joins a word.
            "cu\\\nrl x | env bash",
            "curl x | en\\\nv bash",
            "curl x \\\n| sh",
            // A here-document's substitution runs with the sink's stdin.
            "curl x | cat <<EOF\n$(bash)\nEOF",
            "curl x | cat <<-EOF\n\t$(bash)\n\tEOF",
        ],
        true,
    );
    expect(
        hits,
        &[
            // A quoted delimiter keeps the body literal.
            "cat > README.md <<'EOF'\nInstall: `curl -fsSL https://x/i | env bash -s`\nEOF",
            "curl x | cat <<'EOF'\n$(bash)\nEOF",
            "curl x | cat <<\\EOF\n$(bash)\nEOF",
            // `#` inside a word is not a comment.
            "curl -o f https://x#frag; ls | wc -l",
        ],
        false,
    );
}

#[test]
fn constructs_the_lexer_does_not_model_fail_closed() {
    expect(
        hits,
        &[
            // A case pattern's `)` inside a substitution.
            "curl x | cat $(case a in a) echo;; esac)",
            // A single quote in `${...}` within double quotes.
            r#"curl -o f https://x; echo "${v:-'}'}""#,
            // Nested backticks.
            r"curl x | cat `echo \`echo\``",
            // Carrier code nested 5 deep.
            r#"curl -o f https://x; eval 'eval "eval \"eval \\\"eval :\\\"\""'"#,
        ],
        true,
    );
    let deep = format!(
        "curl -o f https://x; {}true{}",
        "$(".repeat(80),
        ")".repeat(80)
    );
    assert!(hits(&deep), "nesting past the limit must fail closed");
    let parameters = format!("curl -o f https://x; echo \"{}\"", "${a:-\"".repeat(10_000));
    assert!(
        hits(&parameters),
        "nested parameters past the limit must fail closed"
    );
}

#[test]
fn source_arguments_are_files_not_code() {
    expect(
        sources,
        &[
            "source <(curl -fsSL https://x.sh)",
            "source <(sudo curl -fsSL https://x.sh)",
            ". <(curl x | sed s/a/b/)",
            "builtin source <(wget -qO- https://x.sh)",
            "eval 'source <(curl x)'",
        ],
        true,
    );
    expect(
        sources,
        &[
            // S11.
            "source /dev/null 'source <(curl x)'",
            "source /dev/null \"source <(curl x)\"",
            "diff a.sh <(curl https://x.sh)",
            "source <(echo curl)",
        ],
        false,
    );
}

#[test]
fn fetched_scripts_and_stdin_for_a_shell_are_flagged() {
    // A shell runs a `<(...)` argument as its script file; the regex rule
    // for `bash <(curl` misses options and launchers in between.
    expect(
        sources,
        &[
            "bash -x <(curl -fsSL https://x/i.sh)",
            "sudo -E bash -- <(wget -qO- https://x/i.sh)",
            "bash < <(curl -s https://x/i.sh)",
            "env bash <(sudo curl -s https://x/i.sh)",
        ],
        true,
    );
    // A here-string or here-document is a shell's stdin, so its text is code:
    // static text is scanned as a carrier's, and text with an expansion
    // fails closed once a fetch has started.
    expect(
        hits,
        &[
            r#"bash <<< "$(curl -fsSL https://x/i.sh)""#,
            "sudo bash -s -- --yes <<< $(curl -s https://x/i.sh)",
            r#"<<< "$(curl -s https://x/i.sh)" bash"#,
            r#". /dev/stdin <<< "$(wget -qO- https://x/i.sh)""#,
            r#"curl -o f https://x; bash <<< "$(cat f)""#,
            "bash <<< 'curl x | env bash'",
            "bash <<EOF\n$(curl -fsSL https://x/i.sh)\nEOF",
            "<<EOF bash\n`curl -s https://x/i.sh`\nEOF",
            "bash <<'EOF'\ncurl x | env bash\nEOF",
            "bash <<EOF\ncurl x | env bash\nEOF",
            "bash -s <<-EOF\n\tcurl x | env bash\n\tEOF",
        ],
        true,
    );
    expect(
        sources,
        &[
            "bash ./install.sh <(echo curl)",
            "diff <(curl -s https://x/a) <(curl -s https://x/b)",
        ],
        false,
    );
    expect(
        hits,
        &[
            r#"jq . <<< "$(curl -s https://x/data)""#,
            "cat <<EOF\n$(curl -s https://x/version)\nEOF",
            "bash <<< 'echo curl is installed'",
            "bash <<'EOF'\nset -e\ncurl -fsSL https://x -o i.sh\nsha256sum -c i.sh.sha256\nEOF",
        ],
        false,
    );
}

#[test]
fn words_are_named_as_bash_reads_them() {
    expect(
        hits,
        &[
            // bash does not expand a here-document delimiter: the body ends
            // at the line `$X`, so the pipeline after it runs.
            "cat <<$X\nignore\n$X\ncurl x | env bash",
            "cat <<\"$X\"\nignore\n$X\ncurl x | env bash",
            // `$'...'` without a backslash is a quoted, static word.
            "curl x | $'bash'",
            "curl x | $'env' bash",
            // A line continuation joins an assignment, `{` or a reserved word.
            "curl x | X\\\n=1 env bash",
            "curl x | {\\\n env bash; }",
            "curl x | i\\\nf true; then env bash; fi",
            // env -S expands `${NAME}` in its text.
            "curl x | env -S '${SH} -s'",
        ],
        true,
    );
    expect(
        sources,
        &[
            "source <($'curl' -s https://x.sh)",
            ". <($'wget' -qO- https://x.sh)",
        ],
        true,
    );
    expect(
        hits,
        &[
            "curl -s https://x | $'jq' .",
            "curl -s https://x | X\\\n=1 jq .",
        ],
        false,
    );
}

#[test]
fn multibyte_text_in_every_quoting_context() {
    expect(
        hits,
        &[
            "curl x | env bash # é",
            "curl x | cat <<EOF\n日本 $(bash)\nEOF",
            "curl x | cat \"${v:-é}\" $(bash)",
            "eval 'é; curl x | env bash'",
            "curl x | $'b\\u00e9'",
        ],
        true,
    );
    expect(
        hits,
        &[
            "echo 'é' | cat; curl -s https://x | jq '.é'",
            "curl -s https://x | jq \"${v:-é}\"",
            "cat <<'EOF'\n🙂 curl x | env bash\nEOF",
            "curl -s https://x | jq $'.\\u00e9'",
            "curl -s https://x | grep 日本 # 🙂 | env bash",
        ],
        false,
    );
}

#[test]
fn every_command_that_runs_its_stdin_reads_it_as_code() {
    expect(
        hits,
        &[
            // After `--`, a shell's next argument is its -c code.
            "bash -c -- '-x; curl x | env bash'",
            // Shell-starting launchers, su, env -S and computed names.
            "sudo -s <<'EOF'\ncurl x | env bash\nEOF",
            "sudo -i <<< 'curl x | env bash'",
            "su <<'EOF'\ncurl x | env bash\nEOF",
            "env -S 'bash -s' <<< 'curl x | env bash'",
            "\"$SH\" <<'EOF'\ncurl x | env bash\nEOF",
            // A here-document body runs where its `<<` is, before the pipe.
            "cat <<'EOF' | bash\ncurl x | env bash\nEOF",
            "cat <<EOF | env bash\n$(curl -fsSL https://x/i.sh)\nEOF",
            // bash expands `$(cat)` before it redirects a simple command, so
            // only a group's redirection reaches it.
            "{ eval \"$(cat)\"; } <<'EOF'\ncurl x | env bash\nEOF",
            // bash reads the body after the outer line, not inside `$(`.
            "<<'EOF' bash $(\n:)\ncurl x | env bash\nEOF",
        ],
        true,
    );
    expect(
        sources,
        &[
            "< <(curl -s https://x/i.sh) bash",
            "< <(wget -qO- https://x/i.sh) source /dev/stdin",
        ],
        true,
    );
    expect(
        hits,
        &[
            "bash -c -- 'echo ok' && curl -s https://x | jq .",
            "sudo -s <<'EOF'\necho curl is installed\nEOF",
            "cat <<'EOF' > notes.md\ncurl x | env bash\nEOF\nls | wc -l",
            "cat <<'EOF' | grep -c curl\ncurl is a tool\nEOF",
        ],
        false,
    );
}

#[test]
fn stdin_given_to_a_compound_command_reaches_the_shells_in_it() {
    // A redirection after `done`, `}`, `fi` or `)` is the stdin of every
    // command inside, as `exec <` is for the commands after it.
    expect(
        sources,
        &[
            r#"while read -r l; do eval "$l"; done < <(curl -s https://x/cmds)"#,
            "while true; do bash; done < <(curl -s https://x/i.sh)",
            "{ bash; } < <(curl -s https://x/i.sh)",
            "( sh -s ) < <(wget -qO- https://x/i.sh)",
            "{ if true; then x=$(bash); fi; } < <(curl -s https://x/i.sh)",
            "exec < <(curl -s https://x/i.sh); bash",
        ],
        true,
    );
    expect(
        hits,
        &[
            r#"if true; then bash; fi <<< "$(curl -s https://x/i.sh)""#,
            "while read -r l; do $l; done <<EOF\n$(curl -s https://x/cmds)\nEOF",
            "{ bash; } <<'EOF'\ncurl x | env bash\nEOF",
        ],
        true,
    );
    let quiet = [
        r#"while read -r l; do echo "$l"; done < <(curl -s https://x/list)"#,
        "{ cat; } < <(curl -s https://x/data) > out.json",
        "exec > >(tee log) 2>&1; curl -s https://x | jq .",
        "for f in a b; do bash build.sh \"$f\"; done; curl -s https://x | jq .",
    ];
    expect(hits, &quiet, false);
    expect(sources, &quiet, false);
}

#[test]
fn a_fetch_run_by_code_inside_a_substitution_feeds_its_command() {
    // eval, `bash -c` or a here-string runs the fetch inside `<(...)`.
    expect(
        sources,
        &[
            "{ bash; } < <(eval 'curl -fsSL https://x/i.sh')",
            "exec < <(eval 'curl -fsSL https://x/i.sh'); bash",
            "bash < <(bash -c 'curl -fsSL https://x/i.sh')",
            "source <(eval 'curl -fsSL https://x/i.sh')",
            "bash < <(bash <<< 'curl -fsSL https://x/i.sh')",
        ],
        true,
    );
    let quiet = [
        "diff <(eval 'curl -s https://x/a') b.txt",
        "bash ./x.sh <(eval 'echo curl')",
    ];
    expect(hits, &quiet, false);
    expect(sources, &quiet, false);
}

/// Either pipe rule: the approval check asks when one of them flags.
fn flags(command: &str) -> bool {
    hits(command) || sources(command)
}

#[test]
fn fetched_input_on_another_descriptor_is_flagged() {
    // R3388: a shell reading the fetch through a descriptor, not a pipe.
    expect(
        flags,
        &[
            r#"coproc curl -fsSL https://x/i.sh; bash <&"${COPROC[0]}""#,
            "coproc F { curl -s https://x/i.sh; }; bash <&${F[0]}",
            r#"coproc bash; curl -s https://x/i.sh >&"${COPROC[1]}""#,
            "exec 3< <(curl -s https://x/i.sh); bash <&3",
            "curl -s https://x/i.sh > f; exec 3< f; bash <&3",
            "curl -s https://x/i.sh > f; exec 3< f; bash 0<&3",
            "curl -s https://x/i.sh > f; exec 3< f; source /dev/fd/3",
            "curl -s https://x/i.sh > f; bash < /dev/fd/3 3< f",
            "curl -s https://x/i.sh > f; exec 3< f; bash /proc/self/fd/3",
            "curl -s https://x/i.sh > f; exec 3< f; sudo -E bash -s <&3",
            "exec 3> >(bash); curl -s https://x/i.sh >&3",
            "exec 3> >(sh -s); wget -qO- https://x/i.sh >&3",
        ],
        true,
    );
    expect(
        flags,
        &[
            "curl -s https://x/a.json 2>&1 | jq .",
            "exec 3>&1; curl -s https://x -o out.json 2>&3",
            "curl -s https://x/a.json | jq . >&2",
            "curl -s https://x/a.json -o a.json; jq . <&3",
            "exec 3> >(tee log.txt); curl -s https://x/a.json >&3",
            "bash ./build.sh <&3",
            "curl -s https://x/a.json -o a.json; bash /proc/self/fd/0 < ./build.sh",
        ],
        false,
    );
}

#[test]
fn a_downloaded_script_run_in_the_same_command_is_flagged() {
    // R3388: the file a fetch saved, run by a shell, `source`, `.` or its path.
    expect(
        runs,
        &[
            "curl -fsSL https://get.docker.com -o get-docker.sh && sudo sh get-docker.sh",
            "curl -fsSLo install.sh https://x/install.sh && bash install.sh --yes",
            "curl --output=install.sh https://x/i && source install.sh",
            "curl -oinstall.sh https://x/i; bash -x ./install.sh",
            "wget https://x/setup.sh && bash ./setup.sh",
            "curl -O https://x/dl/setup.sh?v=2; sh setup.sh",
            "wget -qO /tmp/i.sh https://x/i && chmod +x /tmp/i.sh && /tmp/i.sh",
            "curl -s https://x/i > i.sh; . ./i.sh",
            "curl -s https://x/i -o i.sh && sudo -E bash -o pipefail i.sh",
            r#"curl -o "$TMP/i.sh" https://x/i && bash "$TMP/i.sh""#,
            r#"curl -o "$TMP/i.sh" https://x/i && "$TMP/i.sh""#,
            "curl -o i.sh https://x/i; eval './i.sh'",
            "eval 'curl -o i.sh https://x/i'; ./i.sh",
            "cd /tmp && curl -LO https://x/i.sh && cd - && bash /tmp/i.sh",
            r#"curl -o i.sh https://x/i && bash "$PWD/i.sh""#,
            "curl -o-i.sh https://x/i && bash -- -i.sh",
            r#"curl -O "https://x/install.sh?token=$TOKEN" && bash install.sh"#,
            r#"wget "https://x/$V/setup.sh" && bash ./setup.sh"#,
            "curl -o i.sh https://x/i && bash -euo pipefail --norc i.sh",
            "curl -o i.sh https://x/i && bash -oe pipefail i.sh",
            ">i.sh curl -fsSL https://x/i && bash i.sh",
            "wget -O - https://x/i.sh > install.sh && bash install.sh",
            "wget -qO- https://x/i.sh > i.sh; bash i.sh",
            "curl -o- https://x/i > i.sh && bash i.sh",
            "curl -fsS -o /dev/stdout https://x/i > i.sh && bash i.sh",
            "wget -O /dev/stdout https://x/i.sh > i.sh; sh i.sh",
            "wget --output-document=- https://x/i.sh > i.sh && bash i.sh",
            "> i.sh wget -nvO- https://x/i.sh; bash i.sh",
            r#"curl -o "$OUT" https://x/i > i.sh && bash i.sh"#,
            "curl -o-i.sh https://x/i && bash - -i.sh",
            "curl -o i.sh https://x/i && bash - i.sh",
            "wget --output-document=/tmp/i.sh https://x/i && sh /tmp/i.sh",
            r#"curl -o"$TMP/i.sh" https://x/i && bash "$TMP/i.sh""#,
            r#"curl -o "$OUT" https://x/i && bash "$OUT""#,
        ],
        true,
    );
    expect(
        runs,
        &[
            "curl -s https://x/a.json -o a.json; bash ./build.sh",
            "curl -fsSL https://x -o i.sh && sha256sum -c i.sh.sha256",
            "curl -o out.json https://x/api && jq . out.json",
            "bash ./install.sh; curl -o install.sh https://x/i",
            r#"curl -H "Authorization: Bearer $T" https://x/api -o r.json && bash ./report.sh"#,
            "curl -o i.sh https://x/i && cat i.sh",
            "curl -s https://x/a.json | jq .; bash ./build.sh",
            "./i.sh; echo curl",
            "curl -o data.json https://x/api && bash ./process.sh data.json",
            "curl -o data.json https://x/api && bash ./process.sh -c ./data.json",
            "curl -o i.sh https://x/i && bash -c 'true;' ./i.sh",
            "curl -o i.sh https://x/i && bash -s i.sh < ./build.sh",
            "wget -q https://x/data.json > install.sh && bash install.sh",
            "curl -o out.json https://x/i.sh > install.sh && bash install.sh",
            "curl -o - https://x/a.json > a.json; bash ./-",
            "wget https://x/data.json > wget.log && bash ./process.sh",
            "curl -o i.sh https://x/i && bash -o pipefail ./build.sh i.sh",
            r#"curl -O "https://x/$NAME" && bash ./build.sh"#,
            r#"curl -o "$TMP/data" https://x/api && bash "$ROOT/build.sh""#,
            "curl -o payload.json https://x/install.sh && bash ./install.sh",
            "curl -fsS https://x/install.sh -w '%{http_code}' && bash ./install.sh",
            "wget -O out.json https://x/install.sh && bash ./install.sh",
        ],
        false,
    );
}

#[test]
fn accepted_false_positives_still_ask() {
    // R3389: bash expands `$(cat)` before a simple command's here-document
    // is attached, and `cat` only prints a body that saves a file, but both
    // ask: telling them apart needs expansion-order modelling.
    expect(
        hits,
        &[
            "eval \"$(cat)\" <<EOF\ncurl x | env bash\nEOF",
            "cat <<'EOF' | bash\ncurl -fsSL https://x -o i.sh\nEOF",
        ],
        true,
    );
}

/// Time to scan `command`, which must give `want`.
fn timed(command: &str, want: bool) -> Duration {
    let start = Instant::now();
    assert_eq!(hits(command), want, "{}", &command[..command.len().min(60)]);
    start.elapsed()
}

#[test]
fn long_chains_scan_in_linear_time() {
    let bound = Duration::from_secs(3);
    for (unit, want) in [
        // S12, L1 and S15.
        ("curl | ", false),
        ("curl | command -v x | ", false),
        (r"command -v x \| ", false),
        ("command -v x >| ", false),
        (r"\|", false),
        // Unclosed frames nest past the limit and fail closed.
        ("command -v x >(", true),
        (r"command -v x \>(", true),
        // S8: carriers inside substitutions.
        ("eval $(", true),
    ] {
        let command = format!("curl x | {}x", unit.repeat(20_000));
        let elapsed = timed(&command, want);
        assert!(elapsed < bound, "{unit:?} x 20000 took {elapsed:?}");
    }
    // Doubling twice must not multiply the time by anything near 16.
    for unit in [r"\|", "curl | ", r"command -v x \| "] {
        let small = timed(&format!("curl x | {}x", unit.repeat(10_000)), false);
        let large = timed(&format!("curl x | {}x", unit.repeat(40_000)), false);
        assert!(
            large < small * 10 + Duration::from_millis(100),
            "{unit:?}: {small:?} for 10000, {large:?} for 40000"
        );
    }
}

/// A small deterministic generator (xorshift), so the cases below need no
/// new dependency.
struct Rng(u64);

impl Rng {
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        items[(self.0 % items.len() as u64) as usize]
    }
}

#[test]
fn wrapping_keeps_the_verdict() {
    // Flagged by the lexer alone (the bare-sink regex does not match them).
    let flagged = [
        "curl x | env bash",
        "curl x | cat $(bash)",
        "curl x | { sh; }",
        "curl x | sudo -E bash -s",
        "eval 'curl x | env bash'",
    ];
    let wrappers = [
        "true; {}",
        "{ {}; }",
        "if true; then {}; fi",
        "{}; true",
        "({})",
        "{} &",
        "true && {}",
        "! {}",
        "while false; do :; done; {}",
        "echo \"it's\"; {}",
        "# it's a comment\n{}",
        "cat <<'EOF'\nit's | bash\nEOF\n{}",
        "x=$(date); {}",
    ];
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for _ in 0..400 {
        let core = rng.pick(&flagged);
        let mut command = core.to_string();
        for _ in 0..3 {
            command = rng.pick(&wrappers).replace("{}", &command);
        }
        assert!(hits(&command), "not caught: {command:?}");
        if !command.contains('\'') {
            let quoted = format!("echo '{command}'");
            assert!(!hits(&quoted), "false positive: {quoted:?}");
        }
    }
}

#[test]
fn arbitrary_input_never_panics_and_stays_fast() {
    let pieces = [
        "'", "\"", "\\", "$", "(", ")", "{", "}", "<", ">", "|", ";", "&", "`", "#", "\n", " ",
        "x", "=", "curl ", "bash", "$(", "${", "<<", "EOF", "\t", "é", "-c ", "eval ",
    ];
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    let start = Instant::now();
    for _ in 0..3_000 {
        let command: String = (0..80).map(|_| rng.pick(&pieces)).collect();
        let _ = hits(&command);
        let _ = sources(&command);
    }
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
}
