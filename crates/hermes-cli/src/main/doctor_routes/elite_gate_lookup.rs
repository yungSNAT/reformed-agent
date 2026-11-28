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

fn parse_shell_env_assignment(token: &str) -> Option<(&str, &str)> {
    token
        .split_once('=')
        .filter(|(name, _)| is_shell_ident(name))
}

fn expand_shell_var(
    rest: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, usize)> {
    if let Some(braced) = rest.strip_prefix('{') {
        let end = braced.find('}')?;
        let name = &braced[..end];
        return is_shell_ident(name).then(|| (lookup(name).unwrap_or_default(), end + 2));
    }
    let len = rest
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let name = &rest[..len];
    is_shell_ident(name).then(|| (lookup(name).unwrap_or_default(), len))
}

fn push_shell_dollar(
    rest: &str,
    out: &mut String,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> usize {
    if let Some((val, used)) = expand_shell_var(rest, lookup) {
        out.push_str(&val);
        1 + used
    } else {
        out.push('$');
        1
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
        (0 | b'"', b'$') => Some(push_shell_dollar(&s[i + 1..], out, lookup)),
        _ => s[i..].chars().next().map(|ch| {
            out.push(ch);
            ch.len_utf8()
        }),
    }
}

fn next_shell_word<'a>(
    input: &'a str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<Option<(String, &'a str)>> {
    let s = input.trim_start();
    if s.is_empty() {
        return Some(None);
    }
    let (bytes, mut i, mut out, mut quote) = (s.as_bytes(), 0, String::new(), 0u8);
    while i < bytes.len() {
        if quote == 0 && bytes[i].is_ascii_whitespace() {
            break;
        }
        i += advance_shell_word(s, i, &mut quote, &mut out, lookup)?;
    }
    (quote == 0).then_some(Some((out, &s[i..])))
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
    fn resolve(&self, name: &str) -> Option<String> {
        if let Some(val) = self.overrides.get(name) {
            return Some(val.clone());
        }
        if name == "PATH" {
            return self
                .base_path
                .and_then(|p| p.to_str())
                .map(ToOwned::to_owned);
        }
        (self.env_lookup)(name)
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
        let Some(next) = next_shell_word(rest, &|name| env.resolve(name)) else {
            return false;
        };
        let Some((token, tail)) = next else {
            break;
        };
        rest = tail;
        if program.is_some() {
            continue;
        }
        if let Some((name, value)) = parse_shell_env_assignment(&token) {
            env.apply_override(name, value);
        } else {
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
