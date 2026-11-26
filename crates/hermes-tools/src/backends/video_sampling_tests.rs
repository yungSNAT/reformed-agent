use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::Duration;

const VIDEO_MODE: &str = "HERMES_TEST_VIDEO_SAMPLING_MODE";

struct FixtureVision;

#[async_trait]
impl VisionBackend for FixtureVision {
    async fn analyze(&self, image_url: &str, question: &str) -> Result<String, ToolError> {
        if question.starts_with("Reject this fixture") {
            return Err(ToolError::ExecutionFailed("fixture vision declined".into()));
        }
        let frame = tokio::fs::read_to_string(image_url).await.unwrap();
        Ok(format!("{frame} | {question}"))
    }
}

#[tokio::test]
async fn frame_sampling_orders_frames_and_cleans_the_work_directory() {
    if let Ok(mode) = std::env::var(VIDEO_MODE) {
        let base = std::env::current_dir().unwrap();
        let backend = VisionFrameSamplingVideoBackend::new(Arc::new(FixtureVision));
        let clip = base.join("clip.mp4");
        if mode == "missing" {
            let error = backend
                .analyze_video(clip.to_str().unwrap(), "Describe the survey", 2)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("failed to execute ffmpeg"));
        } else {
            assert_eq!(mode, "stub");
            let output = backend
                .analyze_video(clip.to_str().unwrap(), "Describe the survey", 2)
                .await
                .unwrap();
            let output: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(output["frames_analyzed"], 2);
            let frames = output["frame_analyses"].as_array().unwrap();
            for (index, description) in ["first", "second"].into_iter().enumerate() {
                assert_eq!(frames[index]["frame_index"], index + 1);
                assert_eq!(
                    frames[index]["analysis"],
                    format!(
                        "{description} | Describe the survey\n\nFrame {}/2: describe key visual events and entities.",
                        index + 1
                    )
                );
            }
            for (name, question, expected) in [
                ("failed.mp4", "Describe the survey", "ffmpeg failed"),
                (
                    "empty.mp4",
                    "Describe the survey",
                    "no frames were extracted",
                ),
                ("clip.mp4", "Reject this fixture", "fixture vision declined"),
            ] {
                let input = base.join(name);
                let error = backend
                    .analyze_video(input.to_str().unwrap(), question, 2)
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains(expected), "{error}");
                assert!(input.is_file());
            }
        }
        assert!(std::fs::read_dir(base.join("scratch"))
            .unwrap()
            .next()
            .is_none());
        assert_eq!(std::fs::read(clip).unwrap(), b"fixture video");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    let empty_bin = home.path().join("empty-bin");
    let scratch = home.path().join("scratch");
    for directory in [&bin, &empty_bin, &scratch] {
        std::fs::create_dir(directory).unwrap();
    }
    for input in ["clip.mp4", "failed.mp4", "empty.mp4"] {
        std::fs::write(home.path().join(input), b"fixture video").unwrap();
    }
    let stub = bin.join("ffmpeg");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        -i) input="$2"; shift ;;
        -frames:v) frames="$2"; shift ;;
    esac
    output="$1"
    shift
done
case "$input" in
    */failed.mp4) exit 1 ;;
    */empty.mp4) exit 0 ;;
    */clip.mp4) [ "$frames" = 2 ] || exit 2 ;;
    *) exit 3 ;;
esac
dir="${output%/*}"
printf second > "$dir/frame-002.jpg"
printf first > "$dir/frame-001.jpg"
printf ignored > "$dir/metadata"
"#,
    )
    .unwrap();
    std::fs::set_permissions(stub, std::fs::Permissions::from_mode(0o700)).unwrap();

    let cwd = std::env::current_dir().unwrap();
    let executable = cwd.join(std::env::args_os().next().expect("test executable"));
    for (mode, search_path) in [("missing", empty_bin), ("stub", bin)] {
        let mut child = Command::new(&executable);
        child
            .env_clear()
            .env(VIDEO_MODE, mode)
            .env("HOME", home.path())
            .env("TMPDIR", &scratch)
            .env("PATH", search_path)
            .current_dir(home.path())
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .args(["--exact", "backends::video::sampling_tests::frame_sampling_orders_frames_and_cleans_the_work_directory", "--nocapture"]);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            child.env("LLVM_PROFILE_FILE", cwd.join(profile));
        }
        let child = tokio::time::timeout(Duration::from_secs(20), child.output())
            .await
            .expect("video fixture child timed out")
            .expect("start video fixture child");
        let stdout = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && stdout.contains("1 passed"),
            "{mode}: {stdout}\n{}",
            String::from_utf8_lossy(&child.stderr)
        );
    }
}
