#[cfg(windows)]
fn process_pathext() -> Option<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| Some(".COM;.EXE;.BAT;.CMD".to_string()))
}

#[cfg(not(windows))]
fn process_pathext() -> Option<String> {
    None
}

fn is_shell_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn split_raw_env_assignment(s: &str) -> Option<(&str, &str)> {
    let eq = s.find('=')?;
    let name = &s[..eq];
    is_shell_ident(name).then_some((name, &s[eq + 1..]))
}

enum VarExpansion {
    Expanded(String, usize),
    Literal,
    Invalid,
}

fn expand_braced_var(rest: &str, lookup: &dyn Fn(&str) -> Option<String>) -> VarExpansion {
    let len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if len == 0 || !rest[len..].starts_with('}') {
        return VarExpansion::Invalid;
    }
    let name = &rest[..len];
    if is_shell_ident(name) {
        VarExpansion::Expanded(lookup(name).unwrap_or_default(), len + 2)
    } else if name.bytes().all(|b| b.is_ascii_digit()) {
        VarExpansion::Expanded(String::new(), len + 2)
    } else {
        VarExpansion::Invalid
    }
}

fn expand_shell_var(rest: &str, lookup: &dyn Fn(&str) -> Option<String>) -> VarExpansion {
    if let Some(braced) = rest.strip_prefix('{') {
        return expand_braced_var(braced, lookup);
    }
    if rest.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        return VarExpansion::Expanded(String::new(), 1);
    }
    let len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let name = &rest[..len];
    if is_shell_ident(name) {
        VarExpansion::Expanded(lookup(name).unwrap_or_default(), len)
    } else {
        VarExpansion::Literal
    }
}

fn push_shell_dollar(
    rest: &str,
    out: &mut String,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<usize> {
    match expand_shell_var(rest, lookup) {
        VarExpansion::Expanded(val, used) => {
            out.push_str(&val);
            Some(1 + used)
        }
        VarExpansion::Literal => {
            out.push('$');
            Some(1)
        }
        VarExpansion::Invalid => None,
    }
}

fn push_shell_escape(rest: &str, in_double: bool, out: &mut String) -> Option<usize> {
    let ch = rest.chars().next()?;
    if in_double && !matches!(ch, '"' | '\\' | '$') {
        out.push('\\');
    }
    out.push(ch);
    Some(1 + ch.len_utf8())
}

fn advance_shell_word(
    s: &str,
    i: usize,
    quote: &mut u8,
    out: &mut String,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<usize> {
    let b = s.as_bytes()[i];
    match (*quote, b) {
        (0, b'\'') | (0, b'"') => {
            *quote = b;
            Some(1)
        }
        (b'\'', b'\'') | (b'"', b'"') => {
            *quote = 0;
            Some(1)
        }
        (0, b'\\') => push_shell_escape(&s[i + 1..], false, out),
        (b'"', b'\\') => push_shell_escape(&s[i + 1..], true, out),
        (0 | b'"', b'$') => push_shell_dollar(&s[i + 1..], out, lookup),
        _ => s[i..].chars().next().map(|ch| {
            out.push(ch);
            ch.len_utf8()
        }),
    }
}

fn read_shell_word<'a>(
    s: &'a str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, &'a str)> {
    let (bytes, mut i, mut out, mut quote) = (s.as_bytes(), 0, String::new(), 0u8);
    while i < bytes.len() {
        if quote == 0 && bytes[i].is_ascii_whitespace() {
            break;
        }
        i += advance_shell_word(s, i, &mut quote, &mut out, lookup)?;
    }
    (quote == 0).then_some((out, &s[i..]))
}

struct ShellLookupEnv<'a> {
    path: Option<std::ffi::OsString>,
    pathext: Option<String>,
    use_pathext: bool,
    overrides: std::collections::HashMap<String, String>,
    base_path: Option<&'a std::ffi::OsStr>,
    env_lookup: &'a dyn Fn(&str) -> Option<String>,
}

impl ShellLookupEnv<'_> {
    fn resolve_outer(&self, name: &str) -> Option<String> {
        if name == "PATH" {
            return self
                .base_path
                .and_then(|p| p.to_str())
                .map(ToOwned::to_owned);
        }
        (self.env_lookup)(name)
    }

    fn resolve_for_prefix(&self, name: &str) -> Option<String> {
        self.overrides
            .get(name)
            .cloned()
            .or_else(|| self.resolve_outer(name))
    }

    fn apply_override(&mut self, name: &str, value: &str) {
        if name == "PATH" {
            self.path = (!value.is_empty()).then(|| std::ffi::OsString::from(value));
        } else if name == "PATHEXT" && self.use_pathext {
            let trimmed = value.trim();
            self.pathext = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        }
        self.overrides.insert(name.to_owned(), value.to_owned());
    }
}

fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn candidate_matches_with_pathext(base: &std::path::Path, pathext: Option<&str>) -> bool {
    if is_executable_file(base) {
        return true;
    }
    let Some(exts) = pathext else {
        return false;
    };
    let base_str = base.as_os_str();
    exts.split(';')
        .map(str::trim)
        .filter(|ext| ext.starts_with('.'))
        .any(|ext| {
            let mut with_ext = base_str.to_os_string();
            with_ext.push(ext);
            is_executable_file(std::path::Path::new(&with_ext))
        })
}

fn resolve_program_candidate(
    program: &str,
    path: Option<&std::ffi::OsStr>,
    pathext: Option<&str>,
) -> bool {
    if program.is_empty() {
        return false;
    }
    let candidate = std::path::Path::new(program);
    if candidate.components().count() > 1 {
        candidate_matches_with_pathext(candidate, pathext)
    } else {
        path.is_some_and(|paths| {
            std::env::split_paths(paths)
                .any(|dir| candidate_matches_with_pathext(&dir.join(program), pathext))
        })
    }
}

fn elite_gate_override_available_with(
    cmdline: &str,
    path_env: Option<&std::ffi::OsStr>,
    pathext: Option<&str>,
    use_pathext: bool,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let mut env = ShellLookupEnv {
        path: path_env.map(ToOwned::to_owned),
        pathext: use_pathext
            .then(|| pathext.map(ToOwned::to_owned))
            .flatten(),
        use_pathext,
        overrides: std::collections::HashMap::new(),
        base_path: path_env,
        env_lookup,
    };
    let mut program = None;
    let mut rest = cmdline;
    loop {
        let s = rest.trim_start();
        if s.is_empty() {
            break;
        }
        if program.is_none() {
            if let Some((name, val_src)) = split_raw_env_assignment(s) {
                let Some((value, tail)) = read_shell_word(val_src, &|k| env.resolve_for_prefix(k))
                else {
                    return false;
                };
                env.apply_override(name, &value);
                rest = tail;
                continue;
            }
        }
        let Some((token, tail)) = read_shell_word(s, &|k| env.resolve_outer(k)) else {
            return false;
        };
        rest = tail;
        if program.is_none() {
            program = Some(token);
        }
    }
    program.is_some_and(|prog| {
        resolve_program_candidate(&prog, env.path.as_deref(), env.pathext.as_deref())
    })
}

fn elite_gate_override_available(cmdline: &str) -> bool {
    let pathext = process_pathext();
    elite_gate_override_available_with(
        cmdline,
        std::env::var_os("PATH").as_deref(),
        pathext.as_deref(),
        cfg!(windows),
        &|name| std::env::var(name).ok(),
    )
}
