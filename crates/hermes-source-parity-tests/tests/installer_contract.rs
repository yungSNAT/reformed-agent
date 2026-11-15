use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct TempTree {
    path: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hermes-installer-contract-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp tree");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct InstallerFixture {
    _temp: TempTree,
    root: PathBuf,
    install_dir: PathBuf,
    home_dir: PathBuf,
    tools_dir: PathBuf,
    invocation_log: PathBuf,
    tar_mark: PathBuf,
}

impl InstallerFixture {
    fn new(label: &str) -> Self {
        let temp = TempTree::new(label);
        let root = temp.path().to_path_buf();
        let install_dir = root.join("bin");
        let home_dir = root.join("home");
        let tools_dir = root.join("tools");
        let invocation_log = root.join("hermes-invocations.log");
        let tar_mark = root.join("tar-ran");

        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&home_dir).expect("create home dir");
        fs::create_dir_all(&tools_dir).expect("create fake tools dir");

        // Serves a fixed archive (none when HERMES_FAKE_ARCHIVE is "missing"),
        // a SHA256SUMS that lists it under every asset
        // name (or a wrong hash, near-miss names or no file, per
        // HERMES_FAKE_SUMS), an empty signature bundle, and a redirect from
        // releases/latest to the tag in HERMES_FAKE_LATEST_TAG (none when
        // that is empty). Logs every URL to HERMES_FAKE_CURL_LOG.
        write_executable(
            &tools_dir.join("curl"),
            r#"#!/usr/bin/env sh
out=""
url=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    out="$2"
    shift 2
  else
    url="$1"
    shift
  fi
done
if [ -z "$out" ]; then
  exit 2
fi
printf '%s\n' "$url" >> "${HERMES_FAKE_CURL_LOG:-/dev/null}"
case "$url" in
  */releases/latest)
    tag="${HERMES_FAKE_LATEST_TAG-v9.9.9}"
    [ -n "$tag" ] || exit 22
    printf 'https://github.com/yungSNAT/reformed-agent/releases/tag/%s' "$tag"
    exit 0
    ;;
esac
mkdir -p "$(dirname "$out")"
case "$url" in
  */SHA256SUMS)
    [ "${HERMES_FAKE_SUMS:-}" = "missing" ] && exit 22
    sum="$(printf 'fake-archive\n' | { sha256sum 2>/dev/null || shasum -a 256; } | awk '{print $1}')"
    [ "${HERMES_FAKE_SUMS:-}" = "wrong" ] && sum="0000000000000000000000000000000000000000000000000000000000000000"
    : > "$out"
    for os in linux-x86_64 linux-aarch64 linux-arm64 macos-x86_64 macos-aarch64 macos-arm64; do
      if [ "${HERMES_FAKE_SUMS:-}" = "other-names" ]; then
        printf '%s  hermes-%s.tar.gz.old\n%s  x-hermes-%s.tar.gz\n' "$sum" "$os" "$sum" "$os" >> "$out"
      else
        printf '%s  hermes-%s.tar.gz\n' "$sum" "$os" >> "$out"
      fi
    done
    ;;
  */SHA256SUMS.sigstore.json) printf '{}\n' > "$out" ;;
  *)
    [ "${HERMES_FAKE_ARCHIVE:-}" = "missing" ] && exit 22
    printf 'fake-archive\n' > "$out"
    ;;
esac
"#,
        )
        .expect("write fake curl");

        write_executable(
            &tools_dir.join("tar"),
            r#"#!/usr/bin/env sh
outdir=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-C" ]; then
    outdir="$2"
    shift 2
  else
    shift
  fi
done
if [ -z "$outdir" ]; then
  exit 2
fi
: > "${HERMES_FAKE_TAR_MARK:?}"
mkdir -p "$outdir"
# An archive that holds no recognised binary.
[ -n "${HERMES_FAKE_TAR_EMPTY:-}" ] && exit 0
cat > "$outdir/hermes-agent-ultra" <<'BIN'
#!/usr/bin/env sh
printf '%s\n' "$*" >> "${HERMES_FAKE_BIN_LOG:?}"
exit 0
BIN
chmod +x "$outdir/hermes-agent-ultra"
"#,
        )
        .expect("write fake tar");

        Self {
            _temp: temp,
            root,
            install_dir,
            home_dir,
            tools_dir,
            invocation_log,
            tar_mark,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join("scripts/install.sh"))
            .arg("--version")
            .arg("v-test")
            .arg("--dir")
            .arg(&self.install_dir)
            .env("HOME", &self.home_dir)
            .env("HERMES_HOME", self.root.join("hermes-home"))
            .env("HERMES_FAKE_BIN_LOG", &self.invocation_log)
            .env("HERMES_FAKE_TAR_MARK", &self.tar_mark)
            .env("HERMES_FAKE_CURL_LOG", self.root.join("curl-urls.log"))
            .env_remove("HERMES_FAKE_LATEST_TAG")
            .env_remove("HERMES_REQUIRE_SIGNATURE")
            .env_remove("HERMES_RELEASE_BASE_URL")
            .env("HERMES_INSTALL_PROBE_TIMEOUT_SECONDS", "3")
            .env("PATH", fixture_path(&self.tools_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, extra_args: &[&str]) -> Output {
        let mut command = self.command();
        for arg in extra_args {
            command.arg(arg);
        }
        command.output().expect("run installer")
    }
}

fn fixture_path(tools_dir: &Path) -> String {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", tools_dir.display(), existing.to_string_lossy())
}

fn write_executable(path: &Path, content: &str) -> io::Result<()> {
    fs::write(path, content)?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn noninteractive_auto_install_skips_post_install_verification() {
    let fixture = InstallerFixture::new("noninteractive-auto");

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("Post-install setup skipped (non-interactive install). Run later:"),
        "{text}"
    );
    assert!(
        text.contains(
            "To run verification during install, pass --setup or set RUN_SETUP_MODE=always."
        ),
        "{text}"
    );
    assert!(
        !text.contains("Running post-install verification"),
        "{text}"
    );
    assert!(
        !fixture.invocation_log.exists(),
        "installed binary should not be invoked in non-interactive auto mode"
    );
}

#[test]
fn explicit_setup_runs_bounded_post_install_probes() {
    let fixture = InstallerFixture::new("explicit-setup");

    let started = Instant::now();
    let output = fixture.run(&["--setup"]);
    let elapsed = started.elapsed();
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        elapsed < Duration::from_secs(5),
        "fast probes must not wait for the full watchdog timeout; elapsed={elapsed:?}\n{text}"
    );
    assert!(text.contains("Running post-install verification"), "{text}");
    assert!(text.contains("Current auth/platform status:"), "{text}");

    let invocations =
        fs::read_to_string(&fixture.invocation_log).expect("installed binary should be invoked");
    assert!(
        invocations.lines().any(|line| line == "doctor"),
        "{invocations}"
    );
    assert!(
        invocations.lines().any(|line| line == "auth status"),
        "{invocations}"
    );
    assert!(
        !invocations.lines().any(|line| line == "setup"),
        "non-TTY explicit setup should run probes but skip interactive setup: {invocations}"
    );
}

#[test]
fn default_install_preserves_existing_upstream_hermes_command() {
    let fixture = InstallerFixture::new("coexist-upstream");
    let upstream_hermes = fixture.install_dir.join("hermes");
    write_executable(
        &upstream_hermes,
        "#!/usr/bin/env sh\necho upstream hermes\n",
    )
    .expect("seed upstream hermes");

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("Legacy hermes alias not installed by default; existing upstream hermes commands are left untouched."),
        "{text}"
    );
    assert!(fixture.install_dir.join("hermes-agent-ultra").is_file());
    assert_eq!(
        fs::read_link(fixture.install_dir.join("hermes-ultra")).expect("primary symlink"),
        PathBuf::from("hermes-agent-ultra")
    );
    assert!(
        fs::symlink_metadata(&upstream_hermes)
            .expect("upstream hermes remains")
            .file_type()
            .is_file(),
        "default installer must not replace upstream hermes with a symlink"
    );
    assert_eq!(
        fs::read_to_string(&upstream_hermes).expect("read upstream hermes"),
        "#!/usr/bin/env sh\necho upstream hermes\n"
    );
}

impl InstallerFixture {
    /// Puts a fake `cosign` on PATH that logs its arguments and exits `status`.
    fn fake_cosign(&self, status: i32) -> PathBuf {
        let log = self.root.join("cosign-args.log");
        write_executable(
            &self.tools_dir.join("cosign"),
            &format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit {status}\n",
                log.display()
            ),
        )
        .expect("write fake cosign");
        log
    }

    /// Puts a fake `cargo` on PATH, so a source build is seen without
    /// building. Returns the file it creates when it runs.
    fn fake_cargo(&self) -> PathBuf {
        let mark = self.root.join("cargo-ran");
        write_executable(
            &self.tools_dir.join("cargo"),
            &format!("#!/usr/bin/env sh\n: > '{}'\nexit 0\n", mark.display()),
        )
        .expect("write fake cargo");
        mark
    }
}

/// Asserts the installer refused before extracting or installing anything.
fn assert_refused(fixture: &InstallerFixture, output: &Output, reason: &str) {
    let text = output_text(output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("Refusing to install:"), "{text}");
    assert!(text.contains(reason), "{text}");
    assert!(!fixture.tar_mark.exists(), "tar ran: {text}");
    assert!(
        !fixture.install_dir.join("hermes-agent-ultra").exists(),
        "{text}"
    );
    assert!(!text.contains("building from source"), "{text}");
}

#[test]
fn archive_is_checked_against_sha256sums_before_extraction() {
    let fixture = InstallerFixture::new("sums-match");

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(text.contains("against SHA256SUMS."), "{text}");
    assert!(fixture.tar_mark.exists(), "{text}");
}

#[test]
fn archive_that_does_not_match_sha256sums_is_refused() {
    let fixture = InstallerFixture::new("sums-wrong");

    let output = fixture
        .command()
        .env("HERMES_FAKE_SUMS", "wrong")
        .output()
        .expect("run installer");

    assert_refused(&fixture, &output, "does not match SHA256SUMS");
}

#[test]
fn release_without_sha256sums_is_refused() {
    let fixture = InstallerFixture::new("sums-missing");

    let output = fixture
        .command()
        .env("HERMES_FAKE_SUMS", "missing")
        .output()
        .expect("run installer");

    assert_refused(&fixture, &output, "could not download SHA256SUMS");
}

#[test]
fn checksum_listed_only_under_other_names_is_refused() {
    let fixture = InstallerFixture::new("sums-other-names");

    let output = fixture
        .command()
        .env("HERMES_FAKE_SUMS", "other-names")
        .output()
        .expect("run installer");

    assert_refused(&fixture, &output, "is not listed in SHA256SUMS");
}

/// The value cosign was given for `flag`, from the fake cosign's log.
fn cosign_arg(log: &Path, flag: &str) -> Option<String> {
    let args = fs::read_to_string(log).expect("cosign was called");
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args.first(), Some(&"verify-blob"), "{args:?}");
    args.iter()
        .position(|arg| *arg == flag)
        .map(|i| args[i + 1].to_string())
}

#[test]
fn cosign_on_path_verifies_sha256sums_against_the_release_workflow() {
    let fixture = InstallerFixture::new("cosign-ok");
    let log = fixture.fake_cosign(0);

    let output = fixture.run(&[]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("Verified the signature on SHA256SUMS."),
        "{text}"
    );
    // Only the requested tag's run of release.yml, so an older signed release
    // cannot stand in for it.
    assert_eq!(
        cosign_arg(&log, "--certificate-identity-regexp").as_deref(),
        Some(
            r"^https://github\.com/yungSNAT/reformed-agent/\.github/workflows/release\.yml@refs/tags/v-test$"
        )
    );
    assert_eq!(
        cosign_arg(&log, "--certificate-oidc-issuer").as_deref(),
        Some("https://token.actions.githubusercontent.com")
    );
}

#[test]
fn latest_is_pinned_to_its_tag_for_downloads_and_the_signature() {
    let fixture = InstallerFixture::new("latest-tag");
    let log = fixture.fake_cosign(0);

    let output = fixture.run(&["--version", "latest"]);
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert_eq!(
        cosign_arg(&log, "--certificate-identity-regexp").as_deref(),
        Some(
            r"^https://github\.com/yungSNAT/reformed-agent/\.github/workflows/release\.yml@refs/tags/v9\.9\.9$"
        )
    );
    let urls = fs::read_to_string(fixture.root.join("curl-urls.log")).expect("curl log");
    let downloads: Vec<&str> = urls
        .lines()
        .filter(|url| !url.ends_with("/releases/latest"))
        .collect();
    assert!(!downloads.is_empty(), "{urls}");
    assert!(
        downloads
            .iter()
            .all(|url| url.contains("/releases/download/v9.9.9/")),
        "{urls}"
    );
}

#[test]
fn unresolved_latest_is_refused_when_cosign_must_check_it() {
    let fixture = InstallerFixture::new("latest-unresolved");
    fixture.fake_cosign(0);

    let output = fixture
        .command()
        .arg("--version")
        .arg("latest")
        .env("HERMES_FAKE_LATEST_TAG", "")
        .output()
        .expect("run installer");

    assert_refused(
        &fixture,
        &output,
        "could not resolve the latest release tag",
    );
}

#[test]
fn signature_that_cosign_rejects_is_refused() {
    let fixture = InstallerFixture::new("cosign-bad");
    fixture.fake_cosign(1);

    let output = fixture.run(&[]);

    assert_refused(&fixture, &output, "is not signed by");
}

#[test]
fn required_signature_without_cosign_is_refused() {
    let fixture = InstallerFixture::new("cosign-required");

    // Only the fake tools and the system folders, so no cosign is on PATH.
    let output = fixture
        .command()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fixture.tools_dir.display()),
        )
        .env("HERMES_REQUIRE_SIGNATURE", "true")
        .output()
        .expect("run installer");

    assert_refused(&fixture, &output, "cosign is not on PATH");
}

#[test]
fn required_signature_refuses_the_source_build_fallback() {
    let fixture = InstallerFixture::new("cosign-required-no-archive");
    fixture.fake_cosign(0);
    let cargo_mark = fixture.fake_cargo();

    let output = fixture
        .command()
        .env("HERMES_FAKE_ARCHIVE", "missing")
        .env("HERMES_REQUIRE_SIGNATURE", "true")
        .output()
        .expect("run installer");

    assert_refused(&fixture, &output, "no signed release archive");
    assert!(!cargo_mark.exists(), "{}", output_text(&output));
}

#[test]
fn required_signature_refuses_a_verified_archive_without_a_binary() {
    let fixture = InstallerFixture::new("cosign-required-empty-archive");
    fixture.fake_cosign(0);
    let cargo_mark = fixture.fake_cargo();

    let output = fixture
        .command()
        .env("HERMES_FAKE_TAR_EMPTY", "1")
        .env("HERMES_REQUIRE_SIGNATURE", "true")
        .output()
        .expect("run installer");
    let text = output_text(&output);

    // The archive passed its checks and was extracted, but held no binary.
    assert!(
        text.contains("Verified the signature on SHA256SUMS."),
        "{text}"
    );
    assert!(fixture.tar_mark.exists(), "{text}");
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("Refusing to install:"), "{text}");
    assert!(text.contains("no signed release archive"), "{text}");
    assert!(!text.contains("building from source"), "{text}");
    assert!(!cargo_mark.exists(), "{text}");
    assert!(
        !fixture.install_dir.join("hermes-agent-ultra").exists(),
        "{text}"
    );
}

#[test]
fn default_home_is_this_agents_own_not_upstreams() {
    let fixture = InstallerFixture::new("default-home");

    let output = fixture
        .command()
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .output()
        .expect("run installer");
    let text = output_text(&output);

    assert!(output.status.success(), "{text}");
    assert!(
        fixture
            .home_dir
            .join(".hermes-agent-ultra/SOUL.md")
            .is_file(),
        "{text}"
    );
    assert!(!fixture.home_dir.join(".hermes").exists(), "{text}");
}
