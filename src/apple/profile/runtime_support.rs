use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::apple::build::toolchain::Toolchain;
use crate::cli::ProfileKind;
use crate::manifest::{ApplePlatform, ProfileManifest, TargetKind};
use crate::util::{copy_file, ensure_dir, run_command};

pub(crate) const TRACE_RUNTIME_LIBRARY_NAME: &str = "libOrbiTrace.dylib";
pub(crate) const TRACE_DEFAULT_RELATIVE_OUTPUT: &str = "Documents/orbi-trace.json";

#[derive(Debug, Clone)]
pub(crate) struct TraceRuntimeLibrary {
    pub(crate) dylib_path: PathBuf,
    pub(crate) library_dir: PathBuf,
    pub(crate) rpath: String,
}

pub(crate) fn compile_trace_runtime_library(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    target_kind: TargetKind,
    output_root: &Path,
    kind: ProfileKind,
) -> Result<TraceRuntimeLibrary> {
    let runtime_dir = output_root.join("orbi-trace-runtime");
    ensure_dir(&runtime_dir)?;
    let dylib_path = runtime_dir.join(TRACE_RUNTIME_LIBRARY_NAME);
    let source_path = trace_runtime_source_path();

    let mut command = toolchain.clang(false);
    command.arg("-target").arg(&toolchain.target_triple);
    command.arg("-isysroot").arg(&toolchain.sdk_path);
    command.args(["-dynamiclib", "-std=c17", "-Wall", "-Wextra", "-Werror"]);
    command.args([
        "-fvisibility=hidden",
        "-install_name",
        "@rpath/libOrbiTrace.dylib",
    ]);
    command.arg(format!(
        "-DORBI_TRACE_DEFAULT_MODE=\"{}\"",
        kind.trace_slug()
    ));
    if profile.is_debug() {
        command.arg("-g");
    } else {
        command.arg("-O2");
    }
    command.arg("-o").arg(&dylib_path);
    command.arg(&source_path);
    run_command(&mut command).with_context(|| {
        format!(
            "failed to compile Orbi trace runtime {}",
            source_path.display()
        )
    })?;

    Ok(TraceRuntimeLibrary {
        dylib_path,
        library_dir: runtime_dir,
        rpath: trace_runtime_rpath(toolchain, target_kind),
    })
}

pub(crate) fn embed_trace_runtime_library(
    runtime: &TraceRuntimeLibrary,
    frameworks_root: &Path,
) -> Result<PathBuf> {
    ensure_dir(frameworks_root)?;
    let destination = frameworks_root.join(TRACE_RUNTIME_LIBRARY_NAME);
    copy_file(&runtime.dylib_path, &destination)?;
    Ok(destination)
}

pub(crate) fn sign_embedded_trace_runtime_if_present(
    bundle_path: &Path,
    signing_identity: &str,
    keychain_path: Option<&Path>,
) -> Result<()> {
    for path in embedded_trace_runtime_candidates(bundle_path) {
        if !path.exists() {
            continue;
        }
        let mut command = Command::new("codesign");
        command.args(["--force", "--sign"]);
        command.arg(signing_identity);
        if let Some(keychain_path) = keychain_path {
            command.args(["--keychain"]);
            command.arg(keychain_path);
        }
        command.arg(&path);
        run_command(&mut command)
            .with_context(|| format!("failed to sign embedded trace runtime {}", path.display()))?;
    }
    Ok(())
}

fn embedded_trace_runtime_candidates(bundle_path: &Path) -> [PathBuf; 2] {
    [
        bundle_path
            .join("Contents")
            .join("Frameworks")
            .join(TRACE_RUNTIME_LIBRARY_NAME),
        bundle_path
            .join("Frameworks")
            .join(TRACE_RUNTIME_LIBRARY_NAME),
    ]
}

fn trace_runtime_source_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("apple")
        .join("profile")
        .join("runtime")
        .join("orbi_trace_runtime.c")
}

fn trace_runtime_rpath(toolchain: &Toolchain, target_kind: TargetKind) -> String {
    if toolchain.platform == ApplePlatform::Macos
        && matches!(
            target_kind,
            TargetKind::App
                | TargetKind::AppExtension
                | TargetKind::WatchApp
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension
        )
    {
        "@executable_path/../Frameworks".to_owned()
    } else {
        "@executable_path/Frameworks".to_owned()
    }
}
