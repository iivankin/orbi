use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};
use serde::Deserialize;
use tempfile::tempdir;

use crate::build::receipt::{BuildReceipt, find_latest_receipt, write_receipt};
use crate::build::toolchain::{DestinationKind, Toolchain};
use crate::cli::{BuildArgs, RunArgs, SubmitArgs};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, DistributionKind, ExtensionManifest, ProfileManifest, SwiftPackageDependency, TargetKind,
    TargetManifest,
};
use crate::util::{
    collect_files_with_extensions, command_output, copy_dir_recursive, copy_file, ensure_dir, ensure_parent_dir,
    prompt_select, resolve_path, run_command,
};

#[derive(Debug, Clone)]
struct BuildRequest {
    target_name: Option<String>,
    profile_name: String,
    destination: Option<DestinationKind>,
    output: Option<PathBuf>,
    provisioning_udids: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct BuiltTarget {
    target_name: String,
    target_kind: TargetKind,
    bundle_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub receipt: BuildReceipt,
    pub receipt_path: PathBuf,
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    let request = BuildRequest {
        target_name: args.target.clone(),
        profile_name: args.profile.clone(),
        destination: resolve_destination(args.simulator, args.device),
        output: args.output.clone(),
        provisioning_udids: None,
    };

    let outcome = build_project(project, &request)?;
    println!("{}", outcome.receipt.artifact_path.display());
    println!("{}", outcome.receipt_path.display());
    Ok(())
}

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    let profile_name = args.profile.clone().unwrap_or_else(|| "development".to_owned());
    let selected_device = if args.device {
        Some(select_physical_device(project, args.device_id.as_deref())?)
    } else {
        None
    };
    let request = BuildRequest {
        target_name: args.target.clone(),
        profile_name,
        destination: resolve_destination(args.simulator, args.device),
        output: None,
        provisioning_udids: selected_device
            .as_ref()
            .map(|device| vec![device.hardware_properties.udid.clone()]),
    };

    let outcome = build_project(project, &request)?;
    match outcome.receipt.destination.as_str() {
        "simulator" => run_on_simulator(project, &outcome.receipt),
        "device" => run_on_device(
            selected_device
                .as_ref()
                .context("device run requested without a selected physical device")?,
            &outcome.receipt,
        ),
        other => bail!("unsupported run destination `{other}`"),
    }
}

pub fn submit_artifact(project: &ProjectContext, args: &SubmitArgs) -> Result<()> {
    let receipt = if let Some(receipt_path) = &args.receipt {
        crate::build::receipt::load_receipt(receipt_path)?
    } else {
        find_latest_receipt(
            &project.project_paths.receipts_dir,
            args.target.as_deref(),
            args.profile.as_deref(),
        )?
        .context("could not find a matching build receipt")?
    };

    if !receipt.submit_eligible {
        bail!(
            "receipt `{}` is not submit-eligible because it was built for `{:?}` distribution",
            receipt.id,
            receipt.distribution
        );
    }

    match receipt.platform {
        ApplePlatform::Ios | ApplePlatform::Tvos | ApplePlatform::Visionos | ApplePlatform::Watchos => {
            submit_with_altool(project, &receipt, args.wait)
        }
        ApplePlatform::Macos => {
            bail!("macOS submit/notarization is not implemented yet")
        }
    }
}

fn build_project(project: &ProjectContext, request: &BuildRequest) -> Result<BuildOutcome> {
    let root_target = project
        .manifest
        .resolve_target(request.target_name.as_deref())?;
    let platform = project
        .manifest
        .resolve_platform_for_target(root_target, None)?;
    let platform_manifest = project
        .manifest
        .platforms
        .get(&platform)
        .context("platform configuration missing from manifest")?;
    let profile = project
        .manifest
        .profile_for(platform, &request.profile_name)?;

    let destination = request
        .destination
        .unwrap_or_else(|| default_destination_for_profile(profile));
    let toolchain = Toolchain::resolve(
        platform,
        platform_manifest.deployment_target.as_str(),
        destination,
    )?;

    let build_root = project
        .project_paths
        .build_dir
        .join(platform.to_string())
        .join(&request.profile_name)
        .join(toolchain.destination.as_str());
    ensure_dir(&build_root)?;

    let ordered_targets = project.manifest.topological_targets(&root_target.name)?;
    let mut built_targets = HashMap::new();
    let signing_required =
        destination == DestinationKind::Device || !matches!(profile.distribution, DistributionKind::Development);
    for target in ordered_targets {
        let built = compile_target(
            project,
            &toolchain,
            target,
            &build_root,
            &request.profile_name,
            profile,
        )?;
        built_targets.insert(target.name.clone(), built);
    }

    if signing_required {
        for target in project
            .manifest
            .topological_targets(&root_target.name)?
            .into_iter()
            .filter(|target| target.name != root_target.name)
        {
            if !target.kind.is_bundle() {
                continue;
            }
            let built = built_targets
                .get(&target.name)
                .with_context(|| format!("missing built target `{}`", target.name))?;
            let material =
                crate::apple::signing::prepare_signing(
                    project,
                    target,
                    platform,
                    profile,
                    request.provisioning_udids.clone(),
                )?;
            crate::apple::signing::sign_bundle(&built.bundle_path, &material)?;
        }
    }

    let built_targets_snapshot = built_targets.clone();
    let root_target_built = built_targets
        .get_mut(&root_target.name)
        .context("root target did not build")?;
    embed_dependencies(
        project,
        &root_target.name,
        &built_targets_snapshot,
        root_target_built,
    )?;

    if signing_required {
        let material = crate::apple::signing::prepare_signing(
            project,
            root_target,
            platform,
            profile,
            request.provisioning_udids.clone(),
        )?;
        crate::apple::signing::sign_bundle(&root_target_built.bundle_path, &material)?;
    }

    let artifact_path = export_artifact(
        project,
        root_target_built,
        &build_root,
        request.output.as_deref(),
        profile,
    )?;

    let receipt = BuildReceipt::new(
        &root_target.name,
        platform,
        &request.profile_name,
        profile.distribution,
        destination.as_str(),
        &root_target.bundle_id,
        root_target_built.bundle_path.clone(),
        artifact_path,
    );
    let receipt_path = write_receipt(&project.project_paths.receipts_dir, &receipt)?;

    Ok(BuildOutcome {
        receipt,
        receipt_path,
    })
}

fn compile_target(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    build_root: &Path,
    profile_name: &str,
    profile: &ProfileManifest,
) -> Result<BuiltTarget> {
    let target_dir = build_root.join(&target.name);
    let intermediates_dir = target_dir.join("intermediates");
    let bundle_root = target_dir.join(bundle_directory_name(target));
    ensure_dir(&intermediates_dir)?;
    ensure_dir(&bundle_root)?;

    let package_outputs = compile_swift_packages(project, toolchain, profile, &intermediates_dir, target)?;
    let c_objects = compile_c_family_sources(project, toolchain, profile, &intermediates_dir, target)?;
    let swift_sources = resolve_target_sources(project, target, &["swift"])?;
    let executable_name = target.name.clone();
    let executable_path = bundle_root.join(&executable_name);

    if !swift_sources.is_empty() {
        compile_swift_target(
            toolchain,
            profile,
            target.kind,
            &swift_sources,
            &package_outputs,
            &c_objects,
            &executable_name,
            &executable_path,
        )?;
    } else if !c_objects.is_empty() {
        link_native_target(toolchain, profile, &c_objects, &executable_path)?;
    } else {
        bail!("target `{}` did not resolve any compilable sources", target.name);
    }

    write_info_plist(project, toolchain, target, &bundle_root, profile_name)?;
    process_resources(project, toolchain, target, &bundle_root)?;

    Ok(BuiltTarget {
        target_name: target.name.clone(),
        target_kind: target.kind,
        bundle_path: bundle_root,
    })
}

fn resolve_destination(simulator: bool, device: bool) -> Option<DestinationKind> {
    if device {
        Some(DestinationKind::Device)
    } else if simulator {
        Some(DestinationKind::Simulator)
    } else {
        None
    }
}

fn default_destination_for_profile(profile: &ProfileManifest) -> DestinationKind {
    match profile.distribution {
        DistributionKind::Development => DestinationKind::Simulator,
        DistributionKind::AdHoc
        | DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => DestinationKind::Device,
    }
}

fn resolve_target_sources(
    project: &ProjectContext,
    target: &TargetManifest,
    extensions: &[&str],
) -> Result<Vec<PathBuf>> {
    let mut sources = Vec::new();
    for root in project.manifest.shared_source_roots() {
        let path = resolve_path(&project.root, &root);
        if path.exists() {
            sources.extend(collect_files_with_extensions(&path, extensions)?);
        }
    }
    for root in &target.sources {
        let path = resolve_path(&project.root, root);
        sources.extend(collect_files_with_extensions(&path, extensions)?);
    }
    sources.sort();
    sources.dedup();
    Ok(sources)
}

fn compile_c_family_sources(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<Vec<PathBuf>> {
    let mut object_files = Vec::new();
    let specs = [
        ("c", false),
        ("m", false),
        ("mm", true),
        ("cpp", true),
        ("cc", true),
        ("cxx", true),
    ];

    for (extension, is_cpp) in specs {
        for source in resolve_target_sources(project, target, &[extension])? {
            let object_name = source
                .file_name()
                .and_then(OsStr::to_str)
                .map(|value| format!("{value}.o"))
                .context("failed to derive object file name")?;
            let object_path = intermediates_dir.join(object_name);
            let mut command = toolchain.clang(is_cpp);
            command.arg("-target").arg(&toolchain.target_triple);
            command.arg("-isysroot").arg(&toolchain.sdk_path);
            command.arg("-c").arg(&source);
            command.arg("-o").arg(&object_path);
            if matches!(profile.configuration.as_str(), "debug") {
                command.arg("-g");
            } else {
                command.arg("-O2");
            }
            run_command(&mut command)?;
            object_files.push(object_path);
        }
    }

    Ok(object_files)
}

fn compile_swift_target(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    target_kind: TargetKind,
    swift_sources: &[PathBuf],
    package_outputs: &[PackageBuildOutput],
    object_files: &[PathBuf],
    module_name: &str,
    executable_path: &Path,
) -> Result<()> {
    let mut command = toolchain.swiftc();
    command.arg("-parse-as-library");
    command.arg("-target").arg(&toolchain.target_triple);
    command.arg("-module-name").arg(module_name);
    command.arg("-o").arg(executable_path);
    if matches!(profile.configuration.as_str(), "debug") {
        command.args(["-Onone", "-g"]);
    } else {
        command.arg("-O");
    }
    if matches!(
        target_kind,
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension
    ) {
        // Extension bundles do not define `main`; the system loader enters through NSExtensionMain.
        command.args(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"]);
    }
    for package in package_outputs {
        command.arg("-I").arg(&package.module_dir);
        command.arg("-L").arg(&package.library_dir);
        for library in &package.link_libraries {
            command.arg("-l").arg(library);
        }
    }
    for object_file in object_files {
        command.arg(object_file);
    }
    for source in swift_sources {
        command.arg(source);
    }
    run_command(&mut command)
}

fn link_native_target(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    object_files: &[PathBuf],
    executable_path: &Path,
) -> Result<()> {
    let mut command = toolchain.clang(false);
    command.arg("-target").arg(&toolchain.target_triple);
    command.arg("-isysroot").arg(&toolchain.sdk_path);
    command.arg("-o").arg(executable_path);
    if matches!(profile.configuration.as_str(), "debug") {
        command.arg("-g");
    } else {
        command.arg("-O2");
    }
    for object_file in object_files {
        command.arg(object_file);
    }
    run_command(&mut command)
}

fn bundle_directory_name(target: &TargetManifest) -> String {
    match target.kind {
        TargetKind::App | TargetKind::WatchApp => format!("{}.app", target.name),
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            format!("{}.appex", target.name)
        }
        _ => target.name.clone(),
    }
}

fn write_info_plist(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
    profile_name: &str,
) -> Result<()> {
    let mut plist = Dictionary::new();
    plist.insert("CFBundleIdentifier".to_owned(), Value::String(target.bundle_id.clone()));
    plist.insert(
        "CFBundleExecutable".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert("CFBundleName".to_owned(), Value::String(target.name.clone()));
    plist.insert(
        "CFBundleDisplayName".to_owned(),
        Value::String(project.manifest.name.clone()),
    );
    plist.insert(
        "CFBundleShortVersionString".to_owned(),
        Value::String(project.manifest.version.clone()),
    );
    plist.insert(
        "CFBundleVersion".to_owned(),
        Value::String(project.manifest.version.clone()),
    );
    plist.insert(
        "CFBundleInfoDictionaryVersion".to_owned(),
        Value::String("6.0".to_owned()),
    );
    plist.insert(
        "CFBundleSupportedPlatforms".to_owned(),
        Value::Array(vec![Value::String(
            toolchain.info_plist_supported_platform().to_owned(),
        )]),
    );

    match target.kind {
        TargetKind::App | TargetKind::WatchApp => {
            plist.insert("CFBundlePackageType".to_owned(), Value::String("APPL".to_owned()));
            plist.insert("LSRequiresIPhoneOS".to_owned(), Value::Boolean(true));
            plist.insert("MinimumOSVersion".to_owned(), Value::String(toolchain.deployment_target.clone()));
        }
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            plist.insert("CFBundlePackageType".to_owned(), Value::String("XPC!".to_owned()));
            plist.insert("MinimumOSVersion".to_owned(), Value::String(toolchain.deployment_target.clone()));
            plist.insert(
                "NSExtension".to_owned(),
                Value::Dictionary(extension_plist(
                    target.extension.as_ref().context("extension configuration missing")?,
                )),
            );
        }
        _ => {
            bail!(
                "target kind `{}` is not implemented yet",
                target.kind.bundle_extension()
            )
        }
    }

    plist.insert(
        "OrbitProfile".to_owned(),
        Value::String(profile_name.to_owned()),
    );

    let path = bundle_root.join("Info.plist");
    ensure_parent_dir(&path)?;
    Value::Dictionary(plist)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn extension_plist(config: &ExtensionManifest) -> Dictionary {
    let mut extension = Dictionary::new();
    extension.insert(
        "NSExtensionPointIdentifier".to_owned(),
        Value::String(config.point_identifier.clone()),
    );
    extension.insert(
        "NSExtensionPrincipalClass".to_owned(),
        Value::String(config.principal_class.clone()),
    );
    extension
}

fn process_resources(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
) -> Result<()> {
    let mut asset_catalogs = Vec::new();
    let mut copy_jobs = Vec::new();

    for resource in &target.resources {
        let resource_path = resolve_path(&project.root, resource);
        if !resource_path.exists() {
            bail!(
                "resource path `{}` for target `{}` does not exist",
                resource_path.display(),
                target.name
            );
        }
        discover_resources(&resource_path, &resource_path, &mut asset_catalogs, &mut copy_jobs)?;
    }

    if !asset_catalogs.is_empty() {
        compile_asset_catalogs(toolchain, &asset_catalogs, bundle_root)?;
    }

    for (source, relative) in copy_jobs {
        let destination = bundle_root.join(relative);
        if source.is_dir() {
            copy_dir_recursive(&source, &destination)?;
        } else {
            copy_file(&source, &destination)?;
        }
    }

    Ok(())
}

fn discover_resources(
    current: &Path,
    root: &Path,
    asset_catalogs: &mut Vec<PathBuf>,
    copy_jobs: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    if current
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
    {
        asset_catalogs.push(current.to_path_buf());
        return Ok(());
    }

    if current.is_file() {
        let relative = current
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| current.file_name().map(PathBuf::from).unwrap_or_default());
        copy_jobs.push((current.to_path_buf(), relative));
        return Ok(());
    }

    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xcassets"))
        {
            asset_catalogs.push(path);
            continue;
        }
        if path.is_dir() {
            discover_resources(&path, root, asset_catalogs, copy_jobs)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("failed to derive resource path for {}", path.display()))?
                .to_path_buf();
            copy_jobs.push((path, relative));
        }
    }
    Ok(())
}

fn compile_asset_catalogs(
    toolchain: &Toolchain,
    asset_catalogs: &[PathBuf],
    bundle_root: &Path,
) -> Result<()> {
    let mut command = toolchain.actool_command();
    command.arg("actool");
    command.arg("--compile").arg(bundle_root);
    command.arg("--platform").arg(toolchain.actool_platform_name());
    command
        .arg("--minimum-deployment-target")
        .arg(&toolchain.deployment_target);
    for device in toolchain.actool_target_device() {
        command.arg("--target-device").arg(device);
    }
    for catalog in asset_catalogs {
        command.arg(catalog);
    }
    command_output(&mut command).map(|_| ())
}

fn embed_dependencies(
    project: &ProjectContext,
    root_target_name: &str,
    built_targets: &HashMap<String, BuiltTarget>,
    root_target: &mut BuiltTarget,
) -> Result<()> {
    let root_manifest = project
        .manifest
        .resolve_target(Some(root_target_name))?;
    for dependency_name in &root_manifest.dependencies {
        let built = built_targets
            .get(dependency_name)
            .with_context(|| format!("missing built dependency `{dependency_name}`"))?;
        let destination = match built.target_kind {
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
                root_target.bundle_path.join("PlugIns").join(
                    built.bundle_path
                        .file_name()
                        .context("dependency bundle name missing")?,
                )
            }
            TargetKind::Framework => root_target.bundle_path.join("Frameworks").join(
                built.bundle_path
                    .file_name()
                    .context("framework bundle name missing")?,
            ),
            _ => continue,
        };
        copy_dir_recursive(&built.bundle_path, &destination)?;
    }
    Ok(())
}

fn export_artifact(
    project: &ProjectContext,
    built_target: &BuiltTarget,
    _build_root: &Path,
    explicit_output: Option<&Path>,
    profile: &ProfileManifest,
) -> Result<PathBuf> {
    match profile.distribution {
        DistributionKind::Development => {
            if let Some(output) = explicit_output {
                let output = resolve_path(&project.root, output);
                if built_target.bundle_path != output {
                    if output.exists() {
                        fs::remove_dir_all(&output).with_context(|| {
                            format!("failed to clear existing output {}", output.display())
                        })?;
                    }
                    copy_dir_recursive(&built_target.bundle_path, &output)?;
                    return Ok(output);
                }
            }
            Ok(built_target.bundle_path.clone())
        }
        DistributionKind::AdHoc | DistributionKind::AppStore => {
            let artifact_name = explicit_output
                .map(Path::to_path_buf)
                .unwrap_or_else(|| {
                    project.project_paths.artifacts_dir.join(format!(
                        "{}-{:?}.ipa",
                        built_target.target_name, profile.distribution
                    ))
                });
            let artifact_path = resolve_path(&project.root, &artifact_name);
            if artifact_path.exists() {
                fs::remove_file(&artifact_path).with_context(|| {
                    format!("failed to remove existing artifact {}", artifact_path.display())
                })?;
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
            copy_dir_recursive(&built_target.bundle_path, &bundle_destination)?;
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
            Ok(artifact_path)
        }
        DistributionKind::DeveloperId | DistributionKind::MacAppStore => {
            bail!("macOS export is not implemented yet")
        }
    }
}

fn run_on_simulator(project: &ProjectContext, receipt: &BuildReceipt) -> Result<()> {
    let device = select_simulator_device(project)?;
    let mut boot = Command::new("xcrun");
    boot.args(["simctl", "boot", &device.udid]);
    let _ = boot.status();

    let mut bootstatus = Command::new("xcrun");
    bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
    run_command(&mut bootstatus)?;

    let mut install = Command::new("xcrun");
    install.args([
        "simctl",
        "install",
        &device.udid,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;

    let mut launch = Command::new("xcrun");
    launch.args([
        "simctl",
        "launch",
        "--console-pty",
        &device.udid,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)?;

    Ok(())
}

fn run_on_device(device: &PhysicalDevice, receipt: &BuildReceipt) -> Result<()> {
    let mut install = Command::new("xcrun");
    install.args([
        "devicectl",
        "device",
        "install",
        "app",
        "--device",
        &device.identifier,
        receipt
            .bundle_path
            .to_str()
            .context("bundle path contains invalid UTF-8")?,
    ]);
    run_command(&mut install)?;

    let mut launch = Command::new("xcrun");
    launch.args([
        "devicectl",
        "device",
        "process",
        "launch",
        "--console",
        "--terminate-existing",
        "--device",
        &device.identifier,
        &receipt.bundle_id,
    ]);
    run_command(&mut launch)
}

fn select_simulator_device(project: &ProjectContext) -> Result<SimulatorDevice> {
    let output = command_output(
        Command::new("xcrun").args(["simctl", "list", "devices", "available", "--json"]),
    )?;
    let devices: SimctlList = serde_json::from_str(&output)?;
    let mut flattened = devices
        .devices
        .into_values()
        .flatten()
        .collect::<Vec<_>>();
    flattened.sort_by(|left, right| left.name.cmp(&right.name));

    if flattened.is_empty() {
        bail!("no available simulators were found");
    }

    let display = flattened
        .iter()
        .map(|device| format!("{} ({})", device.name, device.state))
        .collect::<Vec<_>>();
    let index = if project.app.interactive {
        prompt_select("Select a simulator", &display)?
    } else {
        0
    };
    Ok(flattened.remove(index))
}

fn submit_with_altool(project: &ProjectContext, receipt: &BuildReceipt, wait: bool) -> Result<()> {
    let auth = crate::apple::auth::resolve_submit_auth(&project.app)?;
    let mut command = Command::new("xcrun");
    let mut temp_root_guard = None;
    command.arg("altool");
    command.arg("--upload-package");
    command.arg(&receipt.artifact_path);
    if wait {
        command.arg("--wait");
    }

    match auth {
        crate::apple::auth::SubmitAuth::ApiKey {
            key_id,
            issuer_id,
            api_key_path,
        } => {
            let file_name = api_key_path
                .file_name()
                .context("API key path is missing a file name")?;
            let temp_root = tempdir()?;
            let private_keys_dir = temp_root.path().join("private_keys");
            ensure_dir(&private_keys_dir)?;
            copy_file(&api_key_path, &private_keys_dir.join(file_name))?;
            command.arg("--api-key").arg(key_id);
            command.arg("--api-issuer").arg(issuer_id);
            command.env("API_PRIVATE_KEYS_DIR", &private_keys_dir);
            temp_root_guard = Some(temp_root);
        }
        crate::apple::auth::SubmitAuth::AppleId {
            apple_id,
            password,
            provider_id,
        } => {
            command.arg("--username").arg(apple_id);
            command.arg("--password").arg("@env:ORBIT_ALTOOL_PASSWORD");
            command.env("ORBIT_ALTOOL_PASSWORD", password);
            if let Some(provider_id) = provider_id {
                command.arg("--provider-public-id").arg(provider_id);
            }
        }
    }

    let result = run_command(&mut command);
    drop(temp_root_guard);
    result
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageManifest {
    products: Vec<SwiftPackageProduct>,
    targets: Vec<SwiftPackageTarget>,
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageProduct {
    name: String,
    targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SwiftPackageTarget {
    name: String,
    path: Option<String>,
}

#[derive(Debug, Clone)]
struct PackageBuildOutput {
    module_dir: PathBuf,
    library_dir: PathBuf,
    link_libraries: Vec<String>,
}

fn compile_swift_packages(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<Vec<PackageBuildOutput>> {
    let mut outputs = Vec::new();

    for dependency in &target.swift_packages {
        outputs.push(compile_swift_package(
            project,
            toolchain,
            profile,
            intermediates_dir,
            dependency,
        )?);
    }

    Ok(outputs)
}

fn compile_swift_package(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    intermediates_dir: &Path,
    dependency: &SwiftPackageDependency,
) -> Result<PackageBuildOutput> {
    let package_root = resolve_path(&project.root, &dependency.path);
    let description = command_output(
        Command::new("swift")
            .args(["package", "--package-path"])
            .arg(&package_root)
            .arg("dump-package"),
    )?;
    let package: SwiftPackageManifest = serde_json::from_str(&description).with_context(|| {
        format!(
            "failed to parse Swift package description for {}",
            package_root.display()
        )
    })?;

    let product = package
        .products
        .iter()
        .find(|product| product.name == dependency.product)
        .with_context(|| {
            format!(
                "Swift package `{}` does not export product `{}`",
                package_root.display(),
                dependency.product
            )
        })?;

    if product.targets.len() != 1 {
        bail!(
            "Swift package product `{}` must contain exactly one target for now",
            dependency.product
        );
    }

    let target_name = &product.targets[0];
    let package_target = package
        .targets
        .iter()
        .find(|target| &target.name == target_name)
        .with_context(|| format!("missing Swift package target `{target_name}`"))?;

    let source_root = package_target
        .path
        .as_ref()
        .map(|path| package_root.join(path))
        .unwrap_or_else(|| package_root.join("Sources").join(target_name));
    let swift_sources = collect_files_with_extensions(&source_root, &["swift"])?;
    if swift_sources.is_empty() {
        bail!(
            "Swift package target `{target_name}` under `{}` does not contain any Swift sources",
            source_root.display()
        );
    }

    let module_dir = intermediates_dir.join("swiftmodules").join(&dependency.product);
    let library_dir = intermediates_dir.join("swiftlibs").join(&dependency.product);
    ensure_dir(&module_dir)?;
    ensure_dir(&library_dir)?;

    let module_path = module_dir.join(format!("{}.swiftmodule", dependency.product));
    let library_path = library_dir.join(format!("lib{}.a", dependency.product));
    let mut command = toolchain.swiftc();
    command.arg("-parse-as-library");
    command.arg("-target").arg(&toolchain.target_triple);
    command.arg("-emit-library");
    command.arg("-static");
    command.arg("-emit-module");
    command.arg("-module-name").arg(&dependency.product);
    command.arg("-o").arg(&library_path);
    command.arg("-emit-module-path").arg(&module_path);
    if matches!(profile.configuration.as_str(), "debug") {
        command.args(["-Onone", "-g"]);
    } else {
        command.arg("-O");
    }
    for source in swift_sources {
        command.arg(source);
    }
    run_command(&mut command)?;

    Ok(PackageBuildOutput {
        module_dir,
        library_dir,
        link_libraries: vec![dependency.product.clone()],
    })
}

#[derive(Debug, Clone, Deserialize)]
struct SimctlList {
    devices: BTreeMap<String, Vec<SimulatorDevice>>,
}

#[derive(Debug, Clone, Deserialize)]
struct SimulatorDevice {
    udid: String,
    name: String,
    state: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCtlList {
    result: DeviceCtlResult,
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCtlResult {
    devices: Vec<PhysicalDevice>,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalDevice {
    identifier: String,
    #[serde(rename = "deviceProperties")]
    device_properties: PhysicalDeviceProperties,
    #[serde(rename = "hardwareProperties")]
    hardware_properties: PhysicalHardwareProperties,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalDeviceProperties {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicalHardwareProperties {
    platform: String,
    udid: String,
}

fn select_physical_device(
    project: &ProjectContext,
    requested_identifier: Option<&str>,
) -> Result<PhysicalDevice> {
    let output_path = tempfile::NamedTempFile::new()?;
    let mut list = Command::new("xcrun");
    list.args([
        "devicectl",
        "list",
        "devices",
        "--json-output",
        output_path
            .path()
            .to_str()
            .context("temporary path contains invalid UTF-8")?,
    ]);
    run_command(&mut list)?;
    let contents = fs::read_to_string(output_path.path())
        .with_context(|| format!("failed to read {}", output_path.path().display()))?;
    let devices: DeviceCtlList = serde_json::from_str(&contents)?;
    let mut physical = devices
        .result
        .devices
        .into_iter()
        .filter(|device| device.hardware_properties.platform == "iOS")
        .collect::<Vec<_>>();

    if let Some(identifier) = requested_identifier {
        return physical
            .into_iter()
            .find(|device| {
                device.identifier == identifier
                    || device.hardware_properties.udid == identifier
                    || device.device_properties.name == identifier
            })
            .with_context(|| format!("no connected iOS device matched `{identifier}`"));
    }

    if physical.is_empty() {
        bail!("no connected iOS devices were found through `devicectl`");
    }

    if !project.app.interactive || physical.len() == 1 {
        return Ok(physical.remove(0));
    }

    let labels = physical
        .iter()
        .map(|device| {
            format!(
                "{} ({})",
                device.device_properties.name, device.hardware_properties.udid
            )
        })
        .collect::<Vec<_>>();
    let index = prompt_select("Select a physical device", &labels)?;
    Ok(physical.remove(index))
}
