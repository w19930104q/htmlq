extern crate html5ever;
extern crate kuchikiki;

mod link;
mod pretty_print;

use clap::Parser;
use kuchikiki::NodeRef;
use kuchikiki::traits::*;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

/// Exit code used for every runtime failure reported by htmlq itself.
/// Clap keeps using its conventional exit code 2 for command-line usage errors.
const EXIT_FAILURE: i32 = 1;

#[derive(Debug, Clone, Parser)]
#[command(version, author, about)]
struct Config {
    /// What CSS selector to filter with.
    #[arg(default_value = "html")]
    selector: String,

    /// Where to read HTML input from.
    #[arg(short = 'f', long = "filename", default_value = "-")]
    input_path: String,

    /// Where to write the filtered HTML to.
    #[arg(short = 'o', long = "output", default_value = "-")]
    output_path: String,

    /// What URL to prepend to links without an origin, i.e. starting with a slash (/).
    #[arg(short, long)]
    base: Option<String>,

    /// Look for the `<base>` tag in input for the base.
    #[arg(short = 'B', long)]
    detect_base: bool,

    /// Output only the contained text of the filtered nodes, not the entire HTML.
    #[arg(short, long = "text")]
    text_only: bool,

    /// Skip over text nodes whose text that is solely whitespace.
    #[arg(short, long)]
    ignore_whitespace: bool,

    /// If to reformat the HTML to be more nicely user-readable.
    #[arg(short, long = "pretty")]
    pretty_print: bool,

    /// Do not output the nodes matching any of these selectors.
    #[arg(short, long)]
    remove_nodes: Vec<String>,

    /// Output only the contents of the given attributes.
    #[arg(short, long)]
    attributes: Vec<String>,
}

fn select_attributes(
    node: &NodeRef,
    attributes: &[String],
    output: &mut dyn io::Write,
) -> io::Result<()> {
    if let Some(as_element) = node.as_element() {
        for attr in attributes {
            if let Ok(elem_atts) = as_element.attributes.try_borrow()
                && let Some(val) = elem_atts.get(attr.as_str())
            {
                writeln!(output, "{}", val)?;
            }
        }
    }
    Ok(())
}

fn serialize_text(node: &NodeRef, ignore_whitespace: bool) -> String {
    let mut result = String::new();
    for text_node in node.inclusive_descendants().text_nodes() {
        if ignore_whitespace && text_node.borrow().trim().is_empty() {
            continue;
        }

        result.push_str(&text_node.borrow());

        if ignore_whitespace {
            result.push('\n');
        }
    }

    result
}

/// Write a single matched node using whichever output mode was requested.
/// Any I/O error is propagated instead of being swallowed.
fn render_node(node: &NodeRef, config: &Config, output: &mut dyn io::Write) -> io::Result<()> {
    if !config.attributes.is_empty() {
        select_attributes(node, &config.attributes, output)?;
        return Ok(());
    }

    if config.text_only {
        writeln!(output, "{}", serialize_text(node, config.ignore_whitespace))?;
        return Ok(());
    }

    if config.pretty_print {
        writeln!(output, "{}", pretty_print::pretty_print(node))?;
        return Ok(());
    }

    writeln!(output, "{}", node.to_string())?;
    Ok(())
}

/// Validate the requested output path before any extraction work touches the
/// filesystem. The path itself must not be an existing directory and its
/// parent directory must exist and be usable, because the temporary file is
/// created next to the destination so that the final rename is atomic.
fn validate_output_path(path: &Path) -> io::Result<PathBuf> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_dir()
    {
        return Err(io::Error::other(format!(
            "output path '{}' is a directory",
            path.display()
        )));
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let parent_metadata = fs::metadata(parent).map_err(|err| {
        io::Error::other(format!(
            "cannot write output to '{}': parent directory '{}' is not usable: {err}",
            path.display(),
            parent.display()
        ))
    })?;
    if !parent_metadata.is_dir() {
        return Err(io::Error::other(format!(
            "cannot write output to '{}': '{}' is not a directory",
            path.display(),
            parent.display()
        )));
    }

    Ok(parent.to_path_buf())
}

/// Atomically write `contents` to `path`.
///
/// The data is first written to a uniquely-named temporary file in the same
/// directory as the destination, flushed and synced, and then renamed over
/// the destination. The rename is atomic, so an existing destination keeps
/// its original bytes until the moment the complete new output is in place.
/// On any failure the temporary file is removed and the destination is left
/// untouched.
fn atomic_write(path: &Path, parent: &Path, contents: &[u8]) -> io::Result<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("invalid output path '{}'", path.display())))?;

    let pid = process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);

    let mut collision: Option<io::Error> = None;
    for attempt in 0..16u32 {
        let tmp_path = parent.join(format!(
            ".{}.htmlq-tmp.{pid}.{nanos}.{attempt}",
            file_name.to_string_lossy()
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(mut file) => {
                let outcome = (|| -> io::Result<()> {
                    file.write_all(contents)?;
                    file.flush()?;
                    file.sync_all()?;
                    drop(file);
                    fs::rename(&tmp_path, path)?;
                    Ok(())
                })();

                if let Err(err) = &outcome {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(io::Error::other(format!(
                        "failed to write output file '{}': {err}",
                        path.display()
                    )));
                }
                return Ok(());
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                collision = Some(err);
                continue;
            }
            Err(err) => {
                return Err(io::Error::other(format!(
                    "failed to create temporary output file next to '{}': {err}",
                    path.display()
                )));
            }
        }
    }

    Err(io::Error::other(format!(
        "could not create a unique temporary output file next to '{}': {}",
        path.display(),
        collision
            .map(|err| err.to_string())
            .unwrap_or_else(|| "too many name collisions".to_string())
    )))
}

fn run() -> Result<(), Box<dyn Error>> {
    let config = Config::parse();

    // --- Phase 1: acquire and validate all inputs. Nothing is written yet. ---

    let mut input: Box<dyn io::Read> = match config.input_path.as_ref() {
        "-" => Box::new(io::stdin()),
        file_name => Box::new(
            File::open(file_name)
                .map_err(|err| format!("failed to open input file '{file_name}': {err}"))?,
        ),
    };

    let document = kuchikiki::parse_html()
        .from_utf8()
        .read_from(&mut input)
        .map_err(|err| format!("failed to parse HTML input: {err}"))?;

    // A --base URL supplied on the command line must be valid, even when
    // --detect-base is allowed to take precedence over it.
    let configured_base = config
        .base
        .as_ref()
        .map(|base| Url::parse(base).map_err(|err| format!("invalid --base URL '{base}': {err}")))
        .transpose()?;

    let base: Option<Url> = match (configured_base, config.detect_base) {
        (Some(base), true) => link::detect_base(&document).or(Some(base)),
        (Some(base), false) => Some(base),
        (None, true) => link::detect_base(&document),
        _ => None,
    };

    // Compile the selectors up-front so a broken selector fails the run
    // before any output file is created or truncated.
    document
        .select(&config.selector)
        .map_err(|()| format!("failed to parse CSS selector '{}'", config.selector))?;

    let remove_node_selector = config.remove_nodes.join(",");
    if !remove_node_selector.is_empty() {
        document.select(&remove_node_selector).map_err(|()| {
            format!("failed to parse --remove-nodes CSS selector '{remove_node_selector}'")
        })?;
    }

    let to_file = config.output_path != "-";
    let output_path = Path::new(&config.output_path);
    let output_parent = if to_file {
        Some(validate_output_path(output_path)?)
    } else {
        None
    };

    // --- Phase 2: run the extraction. No destination file is touched. ---

    let matches: Vec<_> = document
        .select(&config.selector)
        .map_err(|()| format!("failed to parse CSS selector '{}'", config.selector))?
        .collect();

    if matches.is_empty() {
        return Err(format!("no elements matched selector '{}'", config.selector).into());
    }

    if !to_file {
        // Stream straight to stdout, but propagate write errors such as a
        // broken pipe instead of pretending the run succeeded.
        let stdout = io::stdout();
        let mut output = BufWriter::new(stdout.lock());
        for matched_noderef in &matches {
            let node = matched_noderef.as_node();

            if let Ok(remove) = node.select_first(&remove_node_selector) {
                remove.as_node().detach();
            }

            if let Some(base) = &base {
                link::rewrite_relative_url(node, base);
            }

            render_node(node, &config, &mut output)?;
        }
        output.flush()?;
        return Ok(());
    }

    // Render the complete result into memory first. Only once every byte has
    // been produced do we create the temporary file and atomically replace
    // the destination, guaranteeing the old file's bytes survive any failure.
    let mut buffer: Vec<u8> = Vec::new();
    for matched_noderef in &matches {
        let node = matched_noderef.as_node();

        if let Ok(remove) = node.select_first(&remove_node_selector) {
            remove.as_node().detach();
        }

        if let Some(base) = &base {
            link::rewrite_relative_url(node, base);
        }

        render_node(node, &config, &mut buffer)?;
    }

    // --- Phase 3: atomic commit. ---
    atomic_write(
        output_path,
        output_parent.as_ref().expect("output parent was validated"),
        &buffer,
    )?;

    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("htmlq: {error}");
        process::exit(EXIT_FAILURE);
    }
}
