use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::context::AppContext;
use crate::util::{
    CliSpinner, command_output, command_output_allow_failure, debug_command, ensure_dir,
    ensure_parent_dir, read_json_file_if_exists, run_command, write_json_file,
};

#[derive(Debug, Serialize)]
pub(crate) struct OrbitSwiftFormatRequest {
    pub working_directory: PathBuf,
    pub configuration_json: Option<String>,
    pub mode: OrbitSwiftFormatMode,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrbitSwiftFormatMode {
    Check,
    Write,
}

#[derive(Debug, Serialize)]
pub(crate) struct OrbitSwiftLintRequest {
    pub working_directory: PathBuf,
    pub configuration_json: Option<String>,
    pub files: Vec<PathBuf>,
    pub compiler_invocations: Vec<OrbitSwiftLintCompilerInvocation>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrbitSwiftLintCompilerInvocation {
    pub arguments: Vec<String>,
    pub source_files: Vec<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SwiftToolBuildInfo {
    binary_path: PathBuf,
}

struct EmbeddedSwiftToolFile {
    relative_path: &'static str,
    contents: &'static str,
}

struct EmbeddedSwiftToolSpec {
    name: &'static str,
    product: &'static str,
    files: &'static [EmbeddedSwiftToolFile],
}

const ORBIT_SWIFT_FORMAT_FILES: &[EmbeddedSwiftToolFile] = &[
    EmbeddedSwiftToolFile {
        relative_path: "Package.swift",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tools/orbit-swift-format/Package.swift"
        )),
    },
    EmbeddedSwiftToolFile {
        relative_path: "Sources/orbit-swift-format/main.swift",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tools/orbit-swift-format/Sources/orbit-swift-format/main.swift"
        )),
    },
];

const ORBIT_SWIFTLINT_FILES: &[EmbeddedSwiftToolFile] = &[
    EmbeddedSwiftToolFile {
        relative_path: "Package.swift",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tools/orbit-swiftlint/Package.swift"
        )),
    },
    EmbeddedSwiftToolFile {
        relative_path: "Sources/orbit-swiftlint/main.swift",
        contents: include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tools/orbit-swiftlint/Sources/orbit-swiftlint/main.swift"
        )),
    },
];

const ORBIT_SWIFT_FORMAT_TOOL: EmbeddedSwiftToolSpec = EmbeddedSwiftToolSpec {
    name: "orbit-swift-format",
    product: "orbit-swift-format",
    files: ORBIT_SWIFT_FORMAT_FILES,
};

const ORBIT_SWIFTLINT_TOOL: EmbeddedSwiftToolSpec = EmbeddedSwiftToolSpec {
    name: "orbit-swiftlint",
    product: "orbit-swiftlint",
    files: ORBIT_SWIFTLINT_FILES,
};

pub(crate) fn run_orbit_swift_format(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftFormatRequest,
) -> Result<()> {
    let request_path = request_root.join("orbit-swift-format-request.json");
    write_json_file(&request_path, request)?;
    let binary_path = ensure_swift_tool_binary(app, &ORBIT_SWIFT_FORMAT_TOOL)?;
    let mut command = Command::new(binary_path);
    command.arg(&request_path);
    run_command(&mut command).with_context(|| "failed to run the Orbit Swift formatter")
}

pub(crate) fn run_orbit_swiftlint(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftLintRequest,
) -> Result<()> {
    let request_path = request_root.join("orbit-swiftlint-request.json");
    write_json_file(&request_path, request)?;
    let binary_path = ensure_swift_tool_binary(app, &ORBIT_SWIFTLINT_TOOL)?;
    let mut command = Command::new(binary_path);
    command.arg(&request_path);
    run_command(&mut command).with_context(|| "failed to run the Orbit Swift linter")
}

fn ensure_swift_tool_binary(app: &AppContext, spec: &EmbeddedSwiftToolSpec) -> Result<PathBuf> {
    let cache_root = app.global_paths.cache_dir.join("swift-tools").join(format!(
        "{}-{}",
        spec.name,
        tool_content_hash(spec)
    ));
    let package_dir = cache_root.join("package");
    let build_info_path = cache_root.join("build-info.json");

    materialize_swift_tool_package(&package_dir, spec)?;
    if let Some(build_info) = read_json_file_if_exists::<SwiftToolBuildInfo>(&build_info_path)?
        && build_info.binary_path.exists()
    {
        return Ok(build_info.binary_path);
    }

    ensure_dir(&cache_root)?;
    let spinner = CliSpinner::new(format!("Building {}", spec.product));
    let binary_path = build_swift_tool(&package_dir, &cache_root, spec)
        .with_context(|| format!("failed to prepare Orbit-managed tool `{}`", spec.product));
    match binary_path {
        Ok(binary_path) => {
            write_json_file(
                &build_info_path,
                &SwiftToolBuildInfo {
                    binary_path: binary_path.clone(),
                },
            )?;
            spinner.finish_clear();
            Ok(binary_path)
        }
        Err(error) => {
            spinner.finish_failure(format!("Failed to build {}", spec.product));
            Err(error)
        }
    }
}

fn build_swift_tool(
    package_dir: &Path,
    cache_root: &Path,
    spec: &EmbeddedSwiftToolSpec,
) -> Result<PathBuf> {
    let scratch_path = cache_root.join("scratch");
    let dependency_cache_path = cache_root.join("dependency-cache");
    ensure_dir(&scratch_path)?;
    ensure_dir(&dependency_cache_path)?;

    let mut build_command = Command::new("swift");
    build_command
        .arg("build")
        .arg("--disable-keychain")
        .arg("--package-path")
        .arg(package_dir)
        .arg("--scratch-path")
        .arg(&scratch_path)
        .arg("--cache-path")
        .arg(&dependency_cache_path)
        .arg("--configuration")
        .arg("release")
        .arg("--product")
        .arg(spec.product);
    let debug = debug_command(&build_command);
    let (success, stdout, stderr) = command_output_allow_failure(&mut build_command)?;
    if !success {
        bail!("`{debug}` failed\nstdout:\n{}\nstderr:\n{}", stdout, stderr);
    }

    let bin_dir = command_output(
        Command::new("swift")
            .arg("build")
            .arg("--disable-keychain")
            .arg("--package-path")
            .arg(package_dir)
            .arg("--scratch-path")
            .arg(&scratch_path)
            .arg("--cache-path")
            .arg(&dependency_cache_path)
            .arg("--configuration")
            .arg("release")
            .arg("--product")
            .arg(spec.product)
            .arg("--show-bin-path"),
    )?;
    let binary_path = PathBuf::from(bin_dir.trim()).join(spec.product);
    if !binary_path.exists() {
        bail!(
            "swift build reported `{}` but the binary was not found at {}",
            spec.product,
            binary_path.display()
        );
    }
    Ok(binary_path)
}

fn materialize_swift_tool_package(package_dir: &Path, spec: &EmbeddedSwiftToolSpec) -> Result<()> {
    for file in spec.files {
        let target_path = package_dir.join(file.relative_path);
        if let Ok(existing) = fs::read_to_string(&target_path)
            && existing == file.contents
        {
            continue;
        }
        ensure_parent_dir(&target_path)?;
        fs::write(&target_path, file.contents)
            .with_context(|| format!("failed to write {}", target_path.display()))?;
    }
    Ok(())
}

fn tool_content_hash(spec: &EmbeddedSwiftToolSpec) -> String {
    let mut hasher = Sha256::new();
    hasher.update(spec.name.as_bytes());
    hasher.update(spec.product.as_bytes());
    for file in spec.files {
        hasher.update(file.relative_path.as_bytes());
        hasher.update(file.contents.as_bytes());
    }
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
