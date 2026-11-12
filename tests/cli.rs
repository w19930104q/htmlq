use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique, automatically-cleaned-up temporary directory for one test.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let unique = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "htmlq-test-{}-{}-{}",
            prefix,
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&path).expect("should create temporary directory");
        TempDir { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const SAMPLE_HTML: &str =
    "<html><head></head><body><div class=\"hi\"><a href=\"/foo/bar\">Hello</a></div></body></html>";

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
    // Compatibility: --attributes output, one line per matching node.
    attribute_output: (
        "<html><head></head><body><a href=\"/a\">A</a><a href=\"/b\">B</a></body></html>",
        ["--attributes", "href", "a"],
        "/a\n/b\n"
    ),
    // Compatibility: --text output.
    text_output: (
        "<html><head></head><body><div>Hello <span>world</span></div></body></html>",
        ["--text", "div"],
        "Hello world\n"
    ),
    // Compatibility: multiple matches each get their own serialized line.
    multiple_matches: (
        "<html><head></head><body><p>one</p><p>two</p></body></html>",
        ["p"],
        "<p>one</p>\n<p>two</p>\n"
    ),
    // Compatibility: no match is an empty, successful result on stdout.
    no_match_stdout: (
        SAMPLE_HTML,
        [".does-not-exist"],
        ""
    ),
);

#[test]
fn pretty_output_is_serialized_html() {
    // The pretty printer's exact whitespace is its own concern; assert the
    // element survives and the run succeeds on stdout.
    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["--pretty", "div"])
        .write_stdin("<html><head></head><body><div><p>Hello</p></div></body></html>")
        .assert()
        .success()
        .stdout(predicate::str::contains("<div>"))
        .stdout(predicate::str::contains("<p>Hello"))
        .stdout(predicate::str::contains("</div>"));
}

#[test]
fn bad_selector_fails_without_touching_output() {
    let temp = TempDir::new("bad-selector");
    let output = temp.child("out.html");
    let original = "precious existing bytes\n";
    fs::write(&output, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["???", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("CSS selector"));

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());
}

#[test]
fn bad_remove_selector_fails_without_touching_output() {
    let temp = TempDir::new("bad-remove-selector");
    let output = temp.child("out.html");
    let original = "do not lose me\n";
    fs::write(&output, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["div", "--remove-nodes", ":::nope", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("CSS selector"));

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());
}

#[test]
fn bad_base_url_fails_without_touching_output() {
    let temp = TempDir::new("bad-base");
    let output = temp.child("out.html");
    let original = "keep me intact\n";
    fs::write(&output, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["--base", "not a url", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("base URL"));

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());
}

#[test]
fn no_match_to_file_leaves_empty_result_and_succeeds() {
    let temp = TempDir::new("no-match-file");
    let output = temp.child("out.html");

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([".does-not-exist", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .success();

    // An empty extraction still atomically publishes an empty file.
    assert_eq!(fs::read(&output).unwrap(), b"");
}

#[test]
fn missing_parent_directory_fails_without_touching_output() {
    let temp = TempDir::new("missing-parent");
    let output = temp.child("nope/out.html");

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["div", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("output"));

    assert!(!output.exists());
}

#[test]
fn output_path_that_is_a_directory_fails() {
    let temp = TempDir::new("output-is-dir");
    let existing = temp.child("out.html");
    let original = "still here\n";
    fs::write(&existing, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["div", "-o", temp.path().to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("directory"));

    assert_eq!(fs::read(&existing).unwrap(), original.as_bytes());
}

#[test]
fn write_failure_in_readonly_directory_preserves_existing_file() {
    let temp = TempDir::new("readonly-dir");
    let output = temp.child("out.html");
    let original = "immutable bytes\n";
    fs::write(&output, original).unwrap();

    let mut permissions = fs::metadata(temp.path()).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(temp.path(), permissions).unwrap();

    // Root bypasses directory permissions, so the scenario cannot be
    // reproduced there. Detect that by probing whether writes are actually
    // denied rather than assuming an identity model.
    let probe = temp.child("privilege-probe");
    if fs::File::create(&probe).is_ok() {
        let _ = fs::remove_file(&probe);
        let mut permissions = fs::metadata(temp.path()).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(temp.path(), permissions).unwrap();
        eprintln!("skipping read-only directory test: directory remains writable");
        return;
    }

    let _output_assert = Command::cargo_bin("htmlq")
        .unwrap()
        .args(["div", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("output"));

    // Restore permissions so TempDir's cleanup can remove the directory.
    let mut permissions = fs::metadata(temp.path()).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(temp.path(), permissions).unwrap();

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());

    // No leftover temporary files.
    let leftovers: Vec<_> = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("htmlq-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary file was not cleaned up");
}

#[test]
fn disk_full_preserves_existing_file_and_cleans_temp() {
    use std::process::Stdio;

    // Reproduce a genuine ENOSPC on a tiny tmpfs mounted inside a user
    // namespace. The capability is not available everywhere, so skip when the
    // sandbox cannot be created rather than faking the error condition.
    let temp = TempDir::new("disk-full");
    let mount_dir = temp.child("tiny");
    fs::create_dir(&mount_dir).unwrap();
    let output = mount_dir.join("out.html");
    let original = "precious bytes that must survive a full disk\n";
    fs::write(&output, original).unwrap();

    let mut payload = String::from("<html><body>");
    for _ in 0..200 {
        payload.push_str("<p>");
        payload.push_str(&"y".repeat(1000));
        payload.push_str("</p>");
    }
    payload.push_str("</body></html>");
    let input_path = temp.child("big.html");
    fs::write(&input_path, payload).unwrap();

    let script = format!(
        "mount -t tmpfs -o size=64k tmpfs {mount} && exec {bin} p -f {input} -o {output}",
        mount = shell_quote(mount_dir.to_str().unwrap()),
        bin = shell_quote(assert_cmd::cargo::cargo_bin("htmlq").to_str().unwrap()),
        input = shell_quote(input_path.to_str().unwrap()),
        output = shell_quote(output.to_str().unwrap()),
    );

    let status = std::process::Command::new("unshare")
        .args(["-Urm", "sh", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    let Ok(status) = status else {
        eprintln!("skipping disk-full test: user namespaces/tmpfs unavailable");
        return;
    };
    if !status.success() && status.code() == Some(127) {
        eprintln!("skipping disk-full test: unshare or mount not available");
        return;
    }

    assert!(
        status.code() == Some(1),
        "a full disk must produce exit code 1, got {status:?}"
    );
    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());

    let leftovers: Vec<_> = fs::read_dir(&mount_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("htmlq-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary file was not cleaned up");
}

/// Minimal single-quote escaping for building an `sh -c` argument.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[test]
fn overwrites_existing_file_atomically() {
    let temp = TempDir::new("overwrite");
    let output = temp.child("out.html");
    fs::write(&output, "old contents that must be replaced\n").unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["div", "-o", output.to_str().unwrap()])
        .write_stdin(SAMPLE_HTML)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        "<div class=\"hi\"><a href=\"/foo/bar\">Hello</a></div>\n"
    );

    let leftovers: Vec<_> = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("htmlq-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary file was left behind");
}

#[test]
fn consecutive_runs_keep_working() {
    let temp = TempDir::new("consecutive");
    let output = temp.child("out.html");

    for (input, expected) in [
        (
            "<html><body><p>first</p></body></html>",
            "<p>first</p>\n",
        ),
        (
            "<html><body><p>second</p></body></html>",
            "<p>second</p>\n",
        ),
        (
            "<html><body><p>third</p></body></html>",
            "<p>third</p>\n",
        ),
    ] {
        Command::cargo_bin("htmlq")
            .unwrap()
            .args(["p", "-o", output.to_str().unwrap()])
            .write_stdin(input)
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&output).unwrap(), expected);
    }
}

#[test]
fn missing_input_file_fails_with_stable_code() {
    let temp = TempDir::new("missing-input");
    let missing = temp.child("missing.html");
    let output = temp.child("out.html");
    let original = "untouched\n";
    fs::write(&output, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args(["-f", missing.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("input"));

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());
}

#[test]
fn unreadable_input_fails_without_touching_output() {
    let temp = TempDir::new("unreadable-input");
    // A directory opens but cannot be read, so the failure happens while the
    // document is being consumed, before any output file is created.
    let input_dir = temp.child("a-directory");
    fs::create_dir(&input_dir).unwrap();
    let output = temp.child("out.html");
    let original = "leave me alone\n";
    fs::write(&output, original).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([
            "-f",
            input_dir.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(
            predicate::str::contains("parse")
                .or(predicate::str::contains("input"))
                .or(predicate::str::contains("directory")),
        );

    assert_eq!(fs::read(&output).unwrap(), original.as_bytes());
}

#[test]
fn malformed_utf8_is_tolerated_like_the_html5_parser() {
    // html5ever's UTF-8 parser replaces invalid bytes instead of failing.
    // Lock in that compatibility: the run succeeds and the output file is
    // published rather than the error being masked as success after truncation.
    let temp = TempDir::new("lenient-utf8");
    let input = temp.child("in.html");
    let output = temp.child("out.html");
    fs::write(&input, [b'<', b'h', b't', b'm', b'l', b'>', 0x80]).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([
            "-f",
            input.to_str().unwrap(),
            "html",
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.exists());
}

#[test]
fn file_input_and_output_round_trip() {
    let temp = TempDir::new("file-io");
    let input = temp.child("in.html");
    let output = temp.child("out.html");
    fs::write(&input, SAMPLE_HTML).unwrap();

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([
            "-f",
            input.to_str().unwrap(),
            "a",
            "-o",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        "<a href=\"/foo/bar\">Hello</a>\n"
    );
}

#[test]
fn base_url_is_applied_to_relative_links() {
    let temp = TempDir::new("base");
    let output = temp.child("out.html");

    Command::cargo_bin("htmlq")
        .unwrap()
        .args([
            "--base",
            "https://example.net/sub/",
            "a",
            "-o",
            output.to_str().unwrap(),
        ])
        .write_stdin(
            "<html><head></head><body><a href=\"/foo/bar\">Hello</a></body></html>",
        )
        .assert()
        .success();

    let written = fs::read_to_string(&output).unwrap();
    assert!(
        written.contains("href=\"https://example.net/foo/bar\""),
        "unexpected output: {written}"
    );
}

#[test]
fn broken_pipe_is_not_reported_as_success() {
    use std::io::Read;
    use std::process::Stdio;

    // A deliberately oversized document so the child keeps writing after the
    // reader closes. A broken pipe must surface as a non-zero exit code rather
    // than being swallowed by `.ok()`.
    let mut input = String::from("<html><body>");
    for index in 0..10_000 {
        input.push_str(&format!("<p>item number {index} with some padding</p>"));
    }
    input.push_str("</body></html>");

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("htmlq"))
        .arg("p")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("should spawn htmlq");

    {
        let mut stdin = child.stdin.take().expect("should have stdin");
        use std::io::Write;
        stdin.write_all(input.as_bytes()).expect("should write stdin");
    }

    // Read only a little, then drop the pipe before the child finishes.
    let mut stdout = child.stdout.take().expect("should have stdout");
    let mut buffer = [0_u8; 64];
    let _ = stdout.read(&mut buffer);
    drop(stdout);

    let status = child.wait().expect("should wait for child");
    assert!(!status.success(), "broken pipe must not exit successfully");
}
