extern crate html5ever;
extern crate kuchikiki;

mod link;
mod pretty_print;

use clap::Parser;
use kuchikiki::traits::*;
use kuchikiki::NodeRef;
use std::error::Error;
use std::fmt;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;
use url::Url;

/// A stable, non-zero process exit code used for every runtime failure.
/// Clap keeps its own exit code (2) for argument parsing/usage errors.
const EXIT_FAILURE: u8 = 1;

/// All errors htmlq itself can produce. Every variant is reported on stderr
/// and turns into the same stable non-zero exit code.
#[derive(Debug)]
enum AppError {
    /// Reading the input document failed.
    Input(String),
    /// The HTML document could not be parsed.
    Parse(String),
    /// A CSS selector (main or --remove-nodes) could not be compiled.
    Selector(String),
    /// A --base URL could not be parsed.
    BaseUrl(String),
    /// The output destination is unusable or writing to it failed.
    Output(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            AppError::Input(message) => format!("Failed to read input: {message}"),
            AppError::Parse(message) => format!("Failed to parse HTML: {message}"),
            AppError::Selector(message) => format!("Failed to parse CSS selector: {message}"),
            AppError::BaseUrl(message) => format!("Failed to parse base URL: {message}"),
            AppError::Output(message) => format!("Failed to write output: {message}"),
        };
        f.write_str(&message)
    }
}

impl Error for AppError {}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        AppError::Output(error.to_string())
    }
}

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
            if let Ok(elem_atts) = as_element.attributes.try_borrow() {
                if let Some(val) = elem_atts.get(attr.as_str()) {
                    writeln!(output, "{}", val)?;
                }
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("htmlq: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn run() -> Result<(), AppError> {
    let config = Config::parse();

    // Phase 1: read and parse the whole input before any output file is
    // touched. A parse failure must leave an existing output file untouched.
    let document = read_document(&config.input_path)?;

    // Phase 2: validate every selector and the base URL up front, while the
    // output file still has its original bytes.
    let base = resolve_base(&document, &config)?;
    let remove_node_selector = compile_remove_selector(&document, &config.remove_nodes)?;

    // For file output, make sure the destination is usable before doing the
    // extraction work. The actual file is only created later, in the same
    // directory as the destination, and atomically renamed into place.
    let output_to_file = config.output_path != "-";
    if output_to_file {
        validate_output_path(Path::new(&config.output_path))?;
    }

    // Phase 3: render the complete result into memory. Nothing has been
    // truncated yet, so any failure here is invisible to the output file.
    let mut rendered: Vec<u8> = Vec::new();
    render_document(&document, &config, &base, &remove_node_selector, &mut rendered)?;

    // Phase 4: publish the result. Stdout writes report their errors (a broken
    // pipe must not look like success); file writes are atomic.
    if output_to_file {
        atomic_write(Path::new(&config.output_path), &rendered)?;
    } else {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle.write_all(&rendered)?;
        handle.flush()?;
    }

    Ok(())
}

fn read_document(input_path: &str) -> Result<NodeRef, AppError> {
    let document = if input_path == "-" {
        let mut input = io::stdin();
        kuchikiki::parse_html()
            .from_utf8()
            .read_from(&mut input)
            .map_err(|error| AppError::Parse(error.to_string()))?
    } else {
        let mut input =
            File::open(input_path).map_err(|error| AppError::Input(error.to_string()))?;
        kuchikiki::parse_html()
            .from_utf8()
            .read_from(&mut input)
            .map_err(|error| AppError::Parse(error.to_string()))?
    };
    Ok(document)
}

fn resolve_base(document: &NodeRef, config: &Config) -> Result<Option<Url>, AppError> {
    let parsed_base = match &config.base {
        Some(base) => Some(
            Url::parse(base).map_err(|error| AppError::BaseUrl(error.to_string()))?,
        ),
        None => None,
    };

    let base = match (parsed_base, config.detect_base) {
        (Some(base), true) => link::detect_base(document).or(Some(base)),
        (Some(base), false) => Some(base),
        (None, true) => link::detect_base(document),
        (None, false) => None,
    };

    Ok(base)
}

fn compile_remove_selector(
    document: &NodeRef,
    remove_nodes: &[String],
) -> Result<String, AppError> {
    let selector = remove_nodes.join(",");
    if !selector.is_empty() {
        // `select` compiles the selector; a document root always exists so an
        // `Err` here means the selector itself is invalid, not "no match".
        // kuchikiki represents selector compile errors as the unit type, so
        // there is no underlying message to include.
        document
            .select(selector.as_str())
            .map_err(|()| AppError::Selector(selector.clone()))?;
    }
    Ok(selector)
}

fn validate_output_path(output_path: &Path) -> Result<(), AppError> {
    if output_path.is_dir() {
        return Err(AppError::Output(format!(
            "{} is a directory",
            output_path.display()
        )));
    }

    let parent = output_path.parent().filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        let metadata = fs::metadata(parent).map_err(|error| {
            AppError::Output(format!(
                "cannot use output directory {}: {error}",
                parent.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(AppError::Output(format!(
                "{} is not a directory",
                parent.display()
            )));
        }
    }

    Ok(())
}

fn render_document(
    document: &NodeRef,
    config: &Config,
    base: &Option<Url>,
    remove_node_selector: &str,
    output: &mut dyn io::Write,
) -> Result<(), AppError> {
    document
        .select(&config.selector)
        .map_err(|()| AppError::Selector(config.selector.clone()))?
        .inspect(|noderef| {
            let Ok(remove) = noderef.as_node().select_first(remove_node_selector) else {
                return;
            };

            remove.as_node().detach();
        })
        .map(|node| {
            if let Some(base) = base {
                link::rewrite_relative_url(node.as_node(), base)
            }
            node
        })
        .try_for_each(|matched_noderef| {
            let node = matched_noderef.as_node();

            if !config.attributes.is_empty() {
                return select_attributes(node, &config.attributes, output);
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
        })?;

    Ok(())
}

/// Write `contents` next to the destination and atomically replace it, so a
/// write failure (disk full, I/O error, ...) leaves the original file's bytes
/// unchanged.
fn atomic_write(output_path: &Path, contents: &[u8]) -> Result<(), AppError> {
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let file_name = output_path
        .file_name()
        .ok_or_else(|| AppError::Output(format!("invalid output path {}", output_path.display())))?;

    let mut temp_path = parent.clone();
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    temp_path.push(format!(
        ".{}.htmlq-tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        unique
    ));

    let result = write_and_rename(&temp_path, output_path, contents);

    if result.is_err() && temp_path.exists() {
        let _ = fs::remove_file(&temp_path);
    }

    result
}

fn write_and_rename(
    temp_path: &Path,
    output_path: &Path,
    contents: &[u8],
) -> Result<(), AppError> {
    let mut file =
        File::create(temp_path).map_err(|error| AppError::Output(error.to_string()))?;
    file.write_all(contents)
        .map_err(|error| AppError::Output(error.to_string()))?;
    file.flush()
        .map_err(|error| AppError::Output(error.to_string()))?;
    file.sync_all()
        .map_err(|error| AppError::Output(error.to_string()))?;
    drop(file);

    fs::rename(temp_path, output_path).map_err(|error| AppError::Output(error.to_string()))?;

    Ok(())
}
