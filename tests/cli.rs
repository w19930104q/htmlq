use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

macro_rules! cmd_success_tests {
    ($($name:ident: $value:expr,)*) => {
    $(
        #[test]
        fn $name(){
            let (stdin, args, expected) = $value;
            Command::cargo_bin("htmlq")
                .unwrap()
                .args(args)
                .write_stdin(stdin)
                .assert()
                .success()
                .stdout(predicate::str::diff(expected));
        }
    )*
    }
}

cmd_success_tests!(
    find_by_class: (
        "<html><head></head><body><div class=\"hi\"><a href=\"/foo/bar\">Hello</a></div></body></html>",
        [".hi"],
        "<div class=\"hi\"><a href=\"/foo/bar\">Hello</a></div>\n"
    ),
    find_by_id: (
        "<html><head></head><body><div id=\"my-id\"><a href=\"/foo/bar\">Hello</a></div></body></html>",
        ["#my-id"],
        "<div id=\"my-id\"><a href=\"/foo/bar\">Hello</a></div>\n"
    ),
    remove_links: (
        "<html><head></head><body><div id=\"my-id\"><a href=\"/foo/bar\">Hello</a></div></body></html>",
        ["#my-id", "--remove-nodes", "a"],
        "<div id=\"my-id\"></div>\n",
    ),
);

const DOC: &str = "<html><head></head><body>\
<div class=\"item\"><a href=\"/one\">One</a></div>\
<div class=\"item\"><a href=\"/two\">Two</a></div>\
</body></html>";

/// A scratch directory unique to this test process. Everything under it is
/// removed when the returned guard drops.
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let unique = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "htmlq-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            unique,
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Every failure mode must report a stable, non-zero exit code (1) and write
/// its diagnostics to stderr rather than panicking with a non-deterministic code.
fn assert_htmlq_failure(args: &[&str], stdin: &str) {
    Command::cargo_bin("htmlq")
        .unwrap()
        .args(args)
        .write_stdin(stdin)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::is_empty().not());
}

#[test]
fn bad_selector_fails_with_stable_exit_code() {
    assert_htmlq_failure(&["::::bad selector::::"], DOC);
}

#[test]
fn bad_remove_selector_fails_with_stable_exit_code() {
    assert_htmlq_failure(&["div", "--remove-nodes", "::::bad::::"], DOC);
}

#[test]
fn bad_base_url_fails_with_stable_exit_code() {
    assert_htmlq_failure(&["--base", "not a url at all", "div"], DOC);
}

#[test]
fn missing_input_file_fails_with_stable_exit_code() {
    let dir = TempDir::new();
    let missing = dir.path().join("does-not-exist.html");
    assert_htmlq_failure(&["-f", missing.to_str().unwrap(), "div"], "");
}

#[test]
fn no_match_fails_with_stable_exit_code() {
    assert_htmlq_failure(&[".does-not-exist"], DOC);
}

#[test]
fn file_output_is_written_atomically() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");
    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), ".item"])
        .write_stdin(DOC)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let contents = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        contents,
        "<div class=\"item\"><a href=\"/one\">One</a></div>\n\
         <div class=\"item\"><a href=\"/two\">Two</a></div>\n"
    );

    // The temporary staging file must not be left behind.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().contains("htmlq-tmp"),
            "temporary file left behind: {name:?}"
        );
    }
}

#[test]
fn bad_selector_keeps_existing_output_file_unchanged() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");
    const ORIGINAL: &str = "precious existing bytes\n";
    std::fs::write(&out, ORIGINAL).unwrap();
    let original_metadata = std::fs::metadata(&out).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), "::::bad::::"])
        .write_stdin(DOC)
        .assert()
        .failure()
        .code(1);

    assert_eq!(std::fs::read_to_string(&out).unwrap(), ORIGINAL);
    let after_metadata = std::fs::metadata(&out).unwrap();
    assert_eq!(original_metadata.len(), after_metadata.len());
    assert!(
        !std::fs::symlink_metadata(&out)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let name = entry.unwrap().file_name();
        assert!(
            !name.to_string_lossy().contains("htmlq-tmp"),
            "temporary file left behind: {name:?}"
        );
    }
}

#[test]
fn no_match_keeps_existing_output_file_unchanged() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");
    const ORIGINAL: &str = "keep me intact\n";
    std::fs::write(&out, ORIGINAL).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), ".nope"])
        .write_stdin(DOC)
        .assert()
        .failure()
        .code(1);

    assert_eq!(std::fs::read_to_string(&out).unwrap(), ORIGINAL);
}

#[test]
fn bad_base_keeps_existing_output_file_unchanged() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");
    const ORIGINAL: &str = "do not touch\n";
    std::fs::write(&out, ORIGINAL).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), "--base", "://bad", "div"])
        .write_stdin(DOC)
        .assert()
        .failure()
        .code(1);

    assert_eq!(std::fs::read_to_string(&out).unwrap(), ORIGINAL);
}

#[test]
fn output_to_missing_directory_fails_cleanly() {
    let dir = TempDir::new();
    let out = dir.path().join("missing-subdir").join("out.html");
    let before = std::fs::read_dir(dir.path()).unwrap().count();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), ".item"])
        .write_stdin(DOC)
        .assert()
        .failure()
        .code(1);

    // Nothing should have been created.
    let after = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(before, after);
}

#[test]
fn disk_write_failure_is_reported_not_swallowed() {
    // Root bypasses permission bits, so the read-only directory would not
    // actually deny writes under a root test run.
    #[cfg(unix)]
    if std::process::id() == 0 || unsafe { libc_euid() } == 0 {
        eprintln!("skipping disk-write test when running as root");
        return;
    }

    let dir = TempDir::new();
    let out = dir.path().join("readonly.html");

    // Make the destination directory read-only so creating the staging file fails.
    let mut permissions = std::fs::metadata(dir.path()).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o500);
    }
    std::fs::set_permissions(dir.path(), permissions).unwrap();

    let output = Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), ".item"])
        .write_stdin(DOC)
        .output()
        .unwrap();

    // Restore permissions first, so the TempDir guard can always clean up.
    let mut permissions = std::fs::metadata(dir.path()).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o700);
    }
    std::fs::set_permissions(dir.path(), permissions).unwrap();

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        !output.stderr.is_empty(),
        "a failure must be reported on stderr"
    );
    assert!(!out.exists(), "no output file should have been created");
}

#[cfg(unix)]
unsafe fn libc_euid() -> u32 {
    // Avoid pulling in a libc crate just for geteuid().
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[test]
fn overwriting_existing_file_replaces_contents_atomically() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");
    std::fs::write(&out, "stale contents that are longer than the new output\n").unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-o", out.to_str().unwrap(), ".item"])
        .write_stdin(DOC)
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "<div class=\"item\"><a href=\"/one\">One</a></div>\n\
         <div class=\"item\"><a href=\"/two\">Two</a></div>\n"
    );
}

#[test]
fn consecutive_runs_to_same_file_are_stable() {
    let dir = TempDir::new();
    let out = dir.path().join("out.html");

    for _ in 0..3 {
        Command::cargo_bin("htmlq")
            .unwrap()
            .args(["-o", out.to_str().unwrap(), ".item"])
            .write_stdin(DOC)
            .assert()
            .success();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "<div class=\"item\"><a href=\"/one\">One</a></div>\n\
             <div class=\"item\"><a href=\"/two\">Two</a></div>\n"
        );
    }
}

#[test]
fn multiple_matches_stream_all_to_stdout() {
    Command::cargo_bin("htmlq")
        .unwrap()
        .args([".item"])
        .write_stdin(DOC)
        .assert()
        .success()
        .stdout(
            "<div class=\"item\"><a href=\"/one\">One</a></div>\n\
             <div class=\"item\"><a href=\"/two\">Two</a></div>\n",
        );
}

#[test]
fn attribute_text_and_pretty_modes_keep_working() {
    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["--attributes", "href", "a"])
        .write_stdin(DOC)
        .assert()
        .success()
        .stdout("/one\n/two\n");

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["--text", ".item"])
        .write_stdin(DOC)
        .assert()
        .success()
        .stdout("One\nTwo\n");

    let pretty = Command::cargo_bin("htmlq")
        .unwrap()
        .args(["--pretty", ".item"])
        .write_stdin(DOC)
        .output()
        .unwrap();
    assert!(pretty.status.success());
    let stdout = String::from_utf8(pretty.stdout).unwrap();
    assert!(stdout.contains("<div"));
    assert!(stdout.contains("One"));
}

#[test]
fn input_file_and_output_file_round_trip() {
    let dir = TempDir::new();
    let input = dir.path().join("in.html");
    let output = dir.path().join("out.html");
    std::fs::write(&input, DOC).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([
            "-f",
            input.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
            ".item",
        ])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "<div class=\"item\"><a href=\"/one\">One</a></div>\n\
         <div class=\"item\"><a href=\"/two\">Two</a></div>\n"
    );
}

#[cfg(unix)]
#[test]
fn broken_pipe_does_not_look_like_success() {
    use std::process::{Command as StdCommand, Stdio};

    // Build a document large enough to keep the producer writing after the
    // consumer closes the pipe.
    let mut big_doc = String::from("<html><body>");
    for index in 0..20_000u32 {
        big_doc.push_str(&format!(
            "<p>line {index} with some padding padding padding</p>"
        ));
    }
    big_doc.push_str("</body></html>");

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("htmlq"))
        .arg("p")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(big_doc.as_bytes()).unwrap();
    }

    // Read only the first chunk, then close stdout immediately.
    {
        let stdout = child.stdout.take().unwrap();
        let mut reader = std::io::BufReader::new(stdout);
        let mut chunk = [0u8; 64];
        let _ = std::io::Read::read(&mut reader, &mut chunk);
    } // stdout handle dropped here

    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "htmlq reported success despite a broken pipe"
    );
}
