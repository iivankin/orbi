use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tempfile::tempdir;

use super::BuiltTarget;
use super::cache::{
    cached_exported_artifact_path, compute_artifact_fingerprint, write_artifact_cache,
};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, DistributionKind, ProfileManifest, TargetKind, TargetManifest,
};
use crate::util::{copy_dir_recursive, copy_file, ensure_dir, resolve_path, run_command};

pub(super) fn export_artifact(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
    signed_bundle_fingerprint: Option<&str>,
) -> Result<std::path::PathBuf> {
    if !matches!(
        built_target.target_kind,
        TargetKind::App | TargetKind::WatchApp
    ) {
        return export_non_app_artifact(project, built_target, explicit_output);
    }
    match profile.distribution {
        DistributionKind::Development => {
            if let Some(output) = explicit_output {
                let output = resolve_path(&project.root, output);
                if built_target.bundle_path != output {
                    remove_existing_path(&output)?;
                    copy_product(&built_target.bundle_path, &output)?;
                    return Ok(output);
                }
            }
            Ok(built_target.bundle_path.clone())
        }
        DistributionKind::AdHoc | DistributionKind::AppStore => {
            let signed_bundle_fingerprint = signed_bundle_fingerprint.with_context(|| {
                format!(
                    "missing signing fingerprint for exported artifact `{}`",
                    built_target.target_name
                )
            })?;
            let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
                project.project_paths.artifacts_dir.join(format!(
                    "{}-{:?}.ipa",
                    built_target.target_name, profile.distribution
                ))
            });
            let artifact_path = resolve_path(&project.root, &artifact_name);
            let artifact_fingerprint =
                compute_artifact_fingerprint(profile.distribution, signed_bundle_fingerprint, None);
            if let Some(cached_artifact) = cached_exported_artifact_path(
                &built_target.target_dir,
                &artifact_path,
                &artifact_fingerprint,
            )? {
                return reuse_cached_artifact(
                    &built_target.target_dir,
                    &artifact_fingerprint,
                    &cached_artifact,
                    &artifact_path,
                );
            }
            if artifact_path.exists() {
                remove_existing_path(&artifact_path)?;
            }
            let payload_dir = tempdir()?;
            let payload_root = payload_dir.path().join("Payload");
            ensure_dir(&payload_root)?;
            let bundle_destination = payload_root.join(
                built_target
                    .bundle_path
                    .file_name()
                    .context("bundle file name missing")?,
            );
            copy_product(&built_target.bundle_path, &bundle_destination)?;
            let mut command = Command::new("ditto");
            command.args([
                "-c",
                "-k",
                "--keepParent",
                payload_root
                    .to_str()
                    .context("payload path contains invalid UTF-8")?,
                artifact_path
                    .to_str()
                    .context("artifact path contains invalid UTF-8")?,
            ]);
            run_command(&mut command)?;
            write_artifact_cache(
                &built_target.target_dir,
                &artifact_fingerprint,
                &artifact_path,
            )?;
            Ok(artifact_path)
        }
        DistributionKind::DeveloperId | DistributionKind::MacAppStore => export_macos_artifact(
            project,
            target,
            platform,
            built_target,
            explicit_output,
            profile,
            signed_bundle_fingerprint,
        ),
    }
}

pub(super) fn remove_existing_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn export_macos_artifact(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
    signed_bundle_fingerprint: Option<&str>,
) -> Result<std::path::PathBuf> {
    if platform != ApplePlatform::Macos {
        bail!("macOS artifact export was requested for non-macOS platform `{platform}`");
    }
    let signed_bundle_fingerprint = signed_bundle_fingerprint.with_context(|| {
        format!(
            "missing signing fingerprint for exported artifact `{}`",
            built_target.target_name
        )
    })?;
    match profile.distribution {
        DistributionKind::MacAppStore => export_signed_macos_app_bundle(
            project,
            built_target,
            explicit_output,
            profile,
            signed_bundle_fingerprint,
        ),
        DistributionKind::DeveloperId => export_signed_macos_disk_image(
            project,
            target,
            platform,
            built_target,
            explicit_output,
            profile,
            signed_bundle_fingerprint,
        ),
        _ => unreachable!("macOS artifact export only handles release macOS distributions"),
    }
}

fn export_signed_macos_app_bundle(
    project: &ProjectContext,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
    signed_bundle_fingerprint: &str,
) -> Result<std::path::PathBuf> {
    let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(format!(
            "{}-{:?}.app",
            built_target.target_name, profile.distribution
        ))
    });
    let artifact_path = resolve_path(&project.root, &artifact_name);
    let artifact_fingerprint =
        compute_artifact_fingerprint(profile.distribution, signed_bundle_fingerprint, None);
    if let Some(cached_artifact) = cached_exported_artifact_path(
        &built_target.target_dir,
        &artifact_path,
        &artifact_fingerprint,
    )? {
        return reuse_cached_artifact(
            &built_target.target_dir,
            &artifact_fingerprint,
            &cached_artifact,
            &artifact_path,
        );
    }

    if built_target.bundle_path != artifact_path {
        remove_existing_path(&artifact_path)?;
        copy_product(&built_target.bundle_path, &artifact_path)?;
    }
    write_artifact_cache(
        &built_target.target_dir,
        &artifact_fingerprint,
        &artifact_path,
    )?;
    Ok(artifact_path)
}

fn export_signed_macos_disk_image(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
    signed_bundle_fingerprint: &str,
) -> Result<std::path::PathBuf> {
    let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(format!(
            "{}-{:?}.dmg",
            built_target.target_name, profile.distribution
        ))
    });
    let artifact_path = resolve_path(&project.root, &artifact_name);
    let signing = crate::apple::signing::prepare_distribution_artifact_signing(
        project,
        &target.bundle_id,
        platform,
        profile,
    )?;
    let artifact_fingerprint = compute_artifact_fingerprint(
        profile.distribution,
        signed_bundle_fingerprint,
        Some(&signing),
    );
    if let Some(cached_artifact) = cached_exported_artifact_path(
        &built_target.target_dir,
        &artifact_path,
        &artifact_fingerprint,
    )? {
        return reuse_cached_artifact(
            &built_target.target_dir,
            &artifact_fingerprint,
            &cached_artifact,
            &artifact_path,
        );
    }

    remove_existing_path(&artifact_path)?;
    create_disk_image(
        &built_target.bundle_path,
        &built_target.target_name,
        &artifact_path,
    )?;
    sign_disk_image(&signing, &artifact_path)?;
    write_artifact_cache(
        &built_target.target_dir,
        &artifact_fingerprint,
        &artifact_path,
    )?;
    Ok(artifact_path)
}

fn create_disk_image(bundle_path: &Path, volume_name: &str, artifact_path: &Path) -> Result<()> {
    let mut command = Command::new("hdiutil");
    command.args(["create", "-fs", "HFS+", "-format", "UDZO", "-volname"]);
    command.arg(volume_name);
    command.arg("-srcfolder").arg(bundle_path);
    command.arg(artifact_path);
    run_command(&mut command)
}

fn sign_disk_image(
    signing: &crate::apple::signing::ArtifactSigningMaterial,
    artifact_path: &Path,
) -> Result<()> {
    let mut command = Command::new("codesign");
    command.args(["--force", "--sign"]);
    command.arg(&signing.signing_identity);
    command.args(["--keychain"]);
    command.arg(&signing.keychain_path);
    command.arg(artifact_path);
    run_command(&mut command)
}

fn export_non_app_artifact(
    project: &ProjectContext,
    built_target: &BuiltTarget,
    explicit_output: Option<&Path>,
) -> Result<std::path::PathBuf> {
    let output = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
        project.project_paths.artifacts_dir.join(
            built_target
                .bundle_path
                .file_name()
                .unwrap_or_else(|| OsStr::new(built_target.target_name.as_str())),
        )
    });
    let output = resolve_path(&project.root, &output);
    if output != built_target.bundle_path {
        remove_existing_path(&output)?;
        copy_product(&built_target.bundle_path, &output)?;
        return Ok(output);
    }
    Ok(built_target.bundle_path.clone())
}

fn copy_product(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        copy_dir_recursive(source, destination)
    } else {
        copy_file(source, destination)
    }
}

fn reuse_cached_artifact(
    target_dir: &Path,
    fingerprint: &str,
    cached_artifact: &Path,
    desired_artifact_path: &Path,
) -> Result<std::path::PathBuf> {
    if cached_artifact != desired_artifact_path {
        remove_existing_path(desired_artifact_path)?;
        copy_product(cached_artifact, desired_artifact_path)?;
    }
    write_artifact_cache(target_dir, fingerprint, desired_artifact_path)?;
    Ok(desired_artifact_path.to_path_buf())
}
