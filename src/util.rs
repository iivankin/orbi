use std::ffi::{OsStr, OsString};
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use dialoguer::{Confirm, Input, Password, Select, theme::ColorfulTheme};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use serde::de::DeserializeOwned;
use walkdir::WalkDir;

pub fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

pub fn prompt_select<T>(prompt: &str, items: &[T]) -> Result<usize>
where
    T: Display,
{
    if items.is_empty() {
        bail!("cannot select from an empty list");
    }

    Select::with_theme(&theme())
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()
        .context("failed to read selection from the terminal")
}

pub fn prompt_confirm(prompt: &str, default: bool) -> Result<bool> {
    Confirm::with_theme(&theme())
        .with_prompt(prompt)
        .default(default)
        .interact()
        .context("failed to read confirmation from the terminal")
}

pub fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String> {
    let dialog_theme = theme();
    let mut input = Input::<String>::with_theme(&dialog_theme);
    input = input.with_prompt(prompt);
    if let Some(value) = default {
        input = input.default(value.to_owned());
    }
    input.interact_text().context("failed to read input")
}

pub fn prompt_password(prompt: &str) -> Result<String> {
    Password::with_theme(&theme())
        .with_prompt(prompt)
        .interact()
        .context("failed to read password")
}

pub struct CliSpinner {
    progress_bar: ProgressBar,
}

impl CliSpinner {
    pub fn new(message: impl Into<String>) -> Self {
        let progress_bar = ProgressBar::new_spinner();
        progress_bar.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg}")
                .expect("spinner template must be valid"),
        );
        progress_bar.enable_steady_tick(std::time::Duration::from_millis(80));
        progress_bar.set_message(message.into());
        progress_bar.tick();
        Self { progress_bar }
    }

    pub fn finish_success(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        print_success(message.into());
    }

    pub fn finish_clear(self) {
        self.progress_bar.finish_and_clear();
    }

    pub fn finish_warning(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        println!("{} {}", yellow_symbol("⚠"), message.into());
    }

    pub fn finish_failure(self, message: impl Into<String>) {
        self.progress_bar.finish_and_clear();
        println!("{} {}", red_symbol("✖"), message.into());
    }
}

pub fn print_success(message: impl AsRef<str>) {
    println!("{} {}", green_symbol("✔"), message.as_ref());
}

fn green_symbol(symbol: &str) -> String {
    format!("\x1b[32m{symbol}\x1b[0m")
}

fn yellow_symbol(symbol: &str) -> String {
    format!("\x1b[33m{symbol}\x1b[0m")
}

fn red_symbol(symbol: &str) -> String {
    format!("\x1b[31m{symbol}\x1b[0m")
}

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("{} does not have a parent directory", path.display());
    };
    ensure_dir(parent)
}

pub fn read_json_file<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn read_json_file_if_exists<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(path).map(Some)
}

pub fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    ensure_parent_dir(path)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

pub fn command_output(command: &mut Command) -> Result<String> {
    let debug = debug_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if !output.status.success() {
        bail!(
            "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("command produced non UTF-8 output")
}

pub fn command_output_allow_failure(command: &mut Command) -> Result<(bool, String, String)> {
    let debug = debug_command(command);
    let output = command
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

pub fn run_command(command: &mut Command) -> Result<()> {
    let debug = debug_command(command);
    let status = command
        .status()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if !status.success() {
        bail!("`{debug}` failed with {status}");
    }
    Ok(())
}

pub fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    ensure_dir(destination)?;
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(source)
            .with_context(|| format!("failed to relativize {}", path.display()))?;
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            ensure_dir(&target)?;
        } else {
            ensure_parent_dir(&target)?;
            fs::copy(path, &target).with_context(|| {
                format!("failed to copy {} to {}", path.display(), target.display())
            })?;
        }
    }
    Ok(())
}

pub fn collect_files_with_extensions(root: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(extension) = entry.path().extension().and_then(OsStr::to_str) else {
            continue;
        };
        if extensions
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

pub fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    ensure_parent_dir(destination)?;
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

pub fn timestamp_slug() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    seconds.to_string()
}

pub fn debug_command(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(os_to_string)
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        program.into_owned()
    } else {
        format!("{program} {args}")
    }
}

pub fn os_to_string(value: &OsStr) -> String {
    shell_escape(value.to_os_string())
}

pub fn shell_escape(value: OsString) -> String {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return "''".to_owned();
    }
    if text
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "/._-=".contains(character))
    {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

pub fn resolve_path(root: &Path, input: &Path) -> PathBuf {
    if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    }
}

pub fn parse_json_str<T>(text: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(text).map_err(|error| anyhow!(error))
}
