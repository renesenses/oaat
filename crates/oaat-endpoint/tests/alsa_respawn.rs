//! #33: when the aplay child exits unexpectedly (DAC unplugged, ALSA error),
//! AlsaDirectOutput must respawn it with bounded backoff instead of dropping
//! every later write, and must not respawn after an intentional stop.
//!
//! A fake `aplay` placed first in PATH stands in for the real one. Its own
//! process (one test binary) keeps the PATH change away from other tests;
//! the scenarios run sequentially in a single #[test] for the same reason.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use oaat_core::format::AudioFormat;
use oaat_endpoint::AlsaDirectOutput;

const FAKE_APLAY: &str = r#"#!/bin/sh
d="$OAAT_FAKE_APLAY_DIR"
echo x >> "$d/calls"
n=$(wc -l < "$d/calls")
case "$(cat "$d/mode")" in
  fail_once) [ "$n" -le 1 ] && exit 1 ;;
  always_fail) exit 1 ;;
esac
exec cat > /dev/null
"#;

fn calls(dir: &Path) -> usize {
    fs::read_to_string(dir.join("calls"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn reset(dir: &Path, mode: &str) {
    let _ = fs::remove_file(dir.join("calls"));
    fs::write(dir.join("mode"), mode).unwrap();
}

/// Write 10 ms of S16 stereo every 10 ms for `dur`; returns how many writes
/// were accepted.
fn pump(out: &mut AlsaDirectOutput, dur: Duration) -> usize {
    let chunk = vec![0u8; 480 * 4];
    let end = Instant::now() + dur;
    let mut accepted = 0;
    while Instant::now() < end {
        if out.write_audio(&chunk) > 0 {
            accepted += 1;
        }
        sleep(Duration::from_millis(10));
    }
    accepted
}

fn new_output() -> AlsaDirectOutput {
    let mut out = AlsaDirectOutput::new();
    out.configure_with_device(AudioFormat::PcmS16le, 48_000, 2, Some("default"))
        .expect("spawn fake aplay");
    out.play();
    out
}

#[test]
fn aplay_is_respawned_after_unexpected_exit_with_bounded_backoff() {
    let dir: PathBuf = std::env::temp_dir().join(format!("oaat-fake-aplay-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("aplay");
    fs::write(&script, FAKE_APLAY).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: single-threaded at this point; this test binary holds one test.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("OAAT_FAKE_APLAY_DIR", &dir);
    }

    // 1. One unexpected exit: playback recovers by itself.
    reset(&dir, "fail_once");
    let mut out = new_output();
    sleep(Duration::from_millis(200)); // let the first child die
    let accepted = pump(&mut out, Duration::from_secs(3));
    assert_eq!(calls(&dir), 2, "exactly one respawn after one exit");
    assert!(accepted > 0, "writes must resume after the respawn");

    // 2. Intentional stop: no respawn, even if play() is called again.
    out.stop();
    let before = calls(&dir);
    out.play();
    assert_eq!(pump(&mut out, Duration::from_secs(1)), 0);
    assert_eq!(calls(&dir), before, "no respawn after an intentional stop");
    drop(out);

    // 3. Persistent failure (DAC gone): retries continue, but with backoff
    //    (250 ms, 500 ms, 1 s, 2 s…), not once per write (~300 in 3 s).
    reset(&dir, "always_fail");
    let mut out = new_output();
    sleep(Duration::from_millis(200));
    assert_eq!(pump(&mut out, Duration::from_secs(3)), 0);
    let n = calls(&dir);
    assert!((3..=6).contains(&n), "bounded retries, got {n} spawns in ~3 s");
    out.stop();

    let _ = fs::remove_dir_all(&dir);
}
