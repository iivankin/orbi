use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};
use serde::Deserialize;
use tempfile::tempdir;

use crate::build::receipt::{BuildReceipt, list_receipts, write_receipt};
use crate::build::toolchain::{DestinationKind, Toolchain};
use crate::cli::{BuildArgs, RunArgs, SubmitArgs};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, DistributionKind, ExtensionManifest, ProfileManifest, SwiftPackageDependency,
    TargetKind, TargetManifest, XcframeworkDependency,
};
use crate::util::{
    CliSpinner, collect_files_with_extensions, command_output, copy_dir_recursive, copy_file,
    ensure_dir, ensure_parent_dir, prompt_select, resolve_path, run_command,
};

#[derive(Debug, Clone)]
struct BuildRequest {
    target_name: String,
    platform: ApplePlatform,
    profile_name: String,
    destination: DestinationKind,
    output: Option<PathBuf>,
    provisioning_udids: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct BuiltTarget {
    target_name: String,
    target_kind: TargetKind,
    bundle_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ExternalLinkInputs {
    module_search_paths: Vec<PathBuf>,
    framework_search_paths: Vec<PathBuf>,
    library_search_paths: Vec<PathBuf>,
    link_frameworks: Vec<String>,
    weak_frameworks: Vec<String>,
    link_libraries: Vec<String>,
    embedded_payloads: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub receipt: BuildReceipt,
    pub receipt_path: PathBuf,
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    let target = resolve_requested_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    let profile_name = resolve_profile_name(
        project,
        platform,
        args.profile.as_deref(),
        None,
        "Select a build profile",
    )?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile_name,
        destination: resolve_destination(project, platform, args.simulator, args.device, profile)?,
        output: args.output.clone(),
        provisioning_udids: None,
    };

    let spinner = CliSpinner::new(format!(
        "Building {} for {} ({})",
        request.target_name,
        request.profile_name,
        request.destination.as_str()
    ));
    let outcome = match build_project(project, &request) {
        Ok(outcome) => outcome,
        Err(error) => {
            spinner.finish_clear();
            return Err(error);
        }
    };
    spinner.finish_success(format!(
        "Built {} for {}.",
        outcome.receipt.target, outcome.receipt.profile
    ));
    println!("artifact: {}", outcome.receipt.artifact_path.display());
    println!("receipt: {}", outcome.receipt_path.display());
    Ok(())
}

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    crate::apple::auth::best_effort_app_store_authenticate(project)?;

    let target = resolve_requested_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    validate_run_platform(platform)?;
    let profile_name = resolve_profile_name(
        project,
        platform,
        args.profile.as_deref(),
        Some("development"),
        "Select a run profile",
    )?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;
    validate_run_distribution(profile)?;
    let destination = resolve_destination(project, platform, args.simulator, args.device, profile)?;
    if args.device_id.is_some() && destination != DestinationKind::Device {
        bail!("--device-id can only be used together with a physical-device run");
    }
    let selected_device = if destination == DestinationKind::Device {
        Some(select_physical_device(project, args.device_id.as_deref())?)
    } else {
        None
    };
    let request = BuildRequest {
        target_name: target.name.clone(),
        platform,
        profile_name,
        destination,
        output: None,
        provisioning_udids: selected_device
            .as_ref()
            .map(|device| vec![device.hardware_properties.udid.clone()]),
    };

    let spinner = CliSpinner::new(format!(
        "Building {} for {} ({})",
        request.target_name,
        request.profile_name,
        request.destination.as_str()
    ));
    let outcome = match build_project(project, &request) {
        Ok(outcome) => outcome,
        Err(error) => {
            spinner.finish_clear();
            return Err(error);
        }
    };
    spinner.finish_success(format!(
        "Built {} for {}.",
        outcome.receipt.target, outcome.receipt.profile
    ));
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
    let receipt = resolve_submit_receipt(project, args)?;

    crate::apple::auth::best_effort_app_store_authenticate(project)?;

    match receipt.platform {
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => submit_with_altool(project, &receipt, args.wait),
        ApplePlatform::Macos => {
            bail!("macOS submit/notarization is not implemented yet")
        }
    }
}

fn build_project(project: &ProjectContext, request: &BuildRequest) -> Result<BuildOutcome> {
    let root_target = project
        .manifest
        .resolve_target(Some(&request.target_name))?;
    let platform = request.platform;
    let platform_manifest = project
        .manifest
        .platforms
        .get(&platform)
        .context("platform configuration missing from manifest")?;
    let profile = project
        .manifest
        .profile_for(platform, &request.profile_name)?;

    let toolchain = Toolchain::resolve(
        platform,
        platform_manifest.deployment_target.as_str(),
        request.destination,
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
    let signing_required = request.destination == DestinationKind::Device
        || !matches!(profile.distribution, DistributionKind::Development);
    if signing_required {
        crate::apple::auth::best_effort_app_store_authenticate(project)?;
    }
    for target in &ordered_targets {
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

    for target in &ordered_targets {
        if target.kind.is_bundle() {
            let built_targets_snapshot = built_targets.clone();
            let built_target = built_targets
                .get_mut(&target.name)
                .with_context(|| format!("missing built target `{}`", target.name))?;
            embed_dependencies(project, target, &built_targets_snapshot, built_target)?;
        }

        if signing_required && target.kind.is_bundle() {
            let built_target = built_targets
                .get(&target.name)
                .with_context(|| format!("missing built target `{}`", target.name))?;
            let material = crate::apple::signing::prepare_signing(
                project,
                target,
                platform,
                profile,
                request.provisioning_udids.clone(),
            )?;
            crate::apple::signing::sign_bundle(&built_target.bundle_path, &material)?;
        }
    }

    let root_target_built = built_targets
        .get(&root_target.name)
        .context("root target did not build")?;
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
        request.destination.as_str(),
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

    let package_outputs =
        compile_swift_packages(project, toolchain, profile, &intermediates_dir, target)?;
    let external_link_inputs =
        resolve_external_link_inputs(project, toolchain, &intermediates_dir, target)?;
    let c_objects =
        compile_c_family_sources(project, toolchain, profile, &intermediates_dir, target)?;
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
            &external_link_inputs,
            &c_objects,
            &executable_name,
            &executable_path,
        )?;
    } else if !c_objects.is_empty() {
        link_native_target(
            toolchain,
            profile,
            &external_link_inputs,
            &c_objects,
            &executable_path,
        )?;
    } else {
        bail!(
            "target `{}` did not resolve any compilable sources",
            target.name
        );
    }

    write_info_plist(project, toolchain, target, &bundle_root, profile_name)?;
    process_resources(project, toolchain, target, &bundle_root)?;
    embed_external_payloads(&external_link_inputs, &bundle_root)?;

    Ok(BuiltTarget {
        target_name: target.name.clone(),
        target_kind: target.kind,
        bundle_path: bundle_root,
    })
}

fn resolve_requested_target<'a>(
    project: &'a ProjectContext,
    requested_target: Option<&str>,
) -> Result<&'a TargetManifest> {
    if let Some(requested_target) = requested_target {
        return project.manifest.resolve_target(Some(requested_target));
    }

    let mut candidates = project.manifest.selectable_root_targets();
    if candidates.len() <= 1 || !project.app.interactive {
        return candidates
            .drain(..)
            .next()
            .context("manifest did not contain any targets");
    }

    let labels = candidates
        .iter()
        .map(|target| format!("{} ({})", target.name, target.bundle_id))
        .collect::<Vec<_>>();
    let index = prompt_select("Select a target", &labels)?;
    Ok(candidates.remove(index))
}

fn resolve_profile_name(
    project: &ProjectContext,
    platform: ApplePlatform,
    requested_profile: Option<&str>,
    default_profile: Option<&str>,
    prompt: &str,
) -> Result<String> {
    if let Some(requested_profile) = requested_profile {
        let _ = project.manifest.profile_for(platform, requested_profile)?;
        return Ok(requested_profile.to_owned());
    }

    if let Some(default_profile) = default_profile {
        if project
            .manifest
            .profile_for(platform, default_profile)
            .is_ok()
        {
            return Ok(default_profile.to_owned());
        }
    }

    let profiles = project.manifest.profile_names(platform)?;
    if profiles.len() == 1 {
        return Ok(profiles[0].clone());
    }
    if !project.app.interactive {
        bail!(
            "multiple profiles are available for platform `{platform}`; pass --profile ({})",
            profiles.join(", ")
        );
    }

    let index = prompt_select(prompt, &profiles)?;
    Ok(profiles[index].clone())
}

fn resolve_destination(
    project: &ProjectContext,
    platform: ApplePlatform,
    simulator: bool,
    device: bool,
    profile: &ProfileManifest,
) -> Result<DestinationKind> {
    if simulator && device {
        bail!("--simulator and --device cannot be used together");
    }
    if platform == ApplePlatform::Macos {
        if simulator {
            bail!("macOS builds do not support `--simulator`");
        }
        return Ok(DestinationKind::Device);
    }
    if device {
        return Ok(DestinationKind::Device);
    }
    if simulator {
        return Ok(DestinationKind::Simulator);
    }
    if matches!(profile.distribution, DistributionKind::Development) && project.app.interactive {
        let options = ["Simulator", "Physical device"];
        let index = prompt_select("Select a destination", &options)?;
        return Ok(match index {
            0 => DestinationKind::Simulator,
            _ => DestinationKind::Device,
        });
    }
    Ok(default_destination_for_profile(platform, profile))
}

fn default_destination_for_profile(
    platform: ApplePlatform,
    profile: &ProfileManifest,
) -> DestinationKind {
    if platform == ApplePlatform::Macos {
        return DestinationKind::Device;
    }

    match profile.distribution {
        DistributionKind::Development => DestinationKind::Simulator,
        DistributionKind::AdHoc
        | DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => DestinationKind::Device,
    }
}

fn validate_run_distribution(profile: &ProfileManifest) -> Result<()> {
    match profile.distribution {
        DistributionKind::Development | DistributionKind::AdHoc => Ok(()),
        DistributionKind::AppStore
        | DistributionKind::DeveloperId
        | DistributionKind::MacAppStore => {
            bail!("`orbit run` only supports development or ad-hoc profiles")
        }
    }
}

fn validate_run_platform(platform: ApplePlatform) -> Result<()> {
    match platform {
        ApplePlatform::Ios => Ok(()),
        ApplePlatform::Macos
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => bail!("`orbit run` is currently implemented for iOS targets"),
    }
}

fn resolve_submit_receipt(project: &ProjectContext, args: &SubmitArgs) -> Result<BuildReceipt> {
    if let Some(receipt_path) = &args.receipt {
        let receipt = crate::build::receipt::load_receipt(receipt_path)?;
        if !receipt.submit_eligible {
            bail!(
                "receipt `{}` is not submit-eligible because it was built for `{:?}` distribution",
                receipt.id,
                receipt.distribution
            );
        }
        return Ok(receipt);
    }

    let mut receipts = list_receipts(
        &project.project_paths.receipts_dir,
        args.target.as_deref(),
        args.profile.as_deref(),
    )?;
    receipts.retain(|receipt| receipt.submit_eligible);
    receipts.sort_by(|left, right| right.created_at_unix.cmp(&left.created_at_unix));
    if receipts.is_empty() {
        bail!("could not find a submit-eligible build receipt");
    }
    if receipts.len() == 1 || !project.app.interactive {
        return Ok(receipts.remove(0));
    }

    let labels = receipts.iter().map(receipt_label).collect::<Vec<_>>();
    let index = prompt_select("Select a build receipt to submit", &labels)?;
    Ok(receipts.remove(index))
}

fn receipt_label(receipt: &BuildReceipt) -> String {
    format!(
        "{} | {} | {} | {} | {}",
        receipt.id,
        receipt.target,
        receipt.profile,
        receipt.destination,
        receipt.artifact_path.display()
    )
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
    external_link_inputs: &ExternalLinkInputs,
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
    apply_external_link_inputs(&mut command, external_link_inputs);
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
    external_link_inputs: &ExternalLinkInputs,
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
    apply_external_link_inputs(&mut command, external_link_inputs);
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
    plist.insert(
        "CFBundleIdentifier".to_owned(),
        Value::String(target.bundle_id.clone()),
    );
    plist.insert(
        "CFBundleExecutable".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert(
        "CFBundleName".to_owned(),
        Value::String(target.name.clone()),
    );
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
        TargetKind::App => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            if matches!(toolchain.platform, ApplePlatform::Ios) {
                plist.insert("LSRequiresIPhoneOS".to_owned(), Value::Boolean(true));
            }
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
        }
        TargetKind::WatchApp => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            plist.insert("WKWatchKitApp".to_owned(), Value::Boolean(true));
            if let Some(companion_bundle_id) =
                parent_bundle_id(project, &target.name, TargetKind::App)
            {
                plist.insert(
                    "WKCompanionAppBundleIdentifier".to_owned(),
                    Value::String(companion_bundle_id),
                );
            }
        }
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("XPC!".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            let mut extension = extension_plist(
                target
                    .extension
                    .as_ref()
                    .context("extension configuration missing")?,
            )?;
            if matches!(target.kind, TargetKind::WatchExtension) {
                let watch_bundle_id = parent_bundle_id(project, &target.name, TargetKind::WatchApp)
                    .context("watch extension must be hosted by a watch app target")?;
                merge_extension_attributes(
                    &mut extension,
                    Dictionary::from_iter([(
                        "WKAppBundleIdentifier".to_owned(),
                        Value::String(watch_bundle_id),
                    )]),
                );
            }
            plist.insert("NSExtension".to_owned(), Value::Dictionary(extension));
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

fn extension_plist(config: &ExtensionManifest) -> Result<Dictionary> {
    let mut extension = Dictionary::new();
    for (key, value) in &config.extra {
        extension.insert(key.clone(), json_to_plist(value)?);
    }
    extension.insert(
        "NSExtensionPointIdentifier".to_owned(),
        Value::String(config.point_identifier.clone()),
    );
    extension.insert(
        "NSExtensionPrincipalClass".to_owned(),
        Value::String(config.principal_class.clone()),
    );
    Ok(extension)
}

fn json_to_plist(value: &serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => bail!("null values are not supported in extension plist extras"),
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Value::Integer(integer.into())
            } else if let Some(float) = value.as_f64() {
                Value::Real(float)
            } else {
                bail!("JSON number `{value}` is not representable in a plist");
            }
        }
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(values) => Value::Array(
            values
                .iter()
                .map(json_to_plist)
                .collect::<Result<Vec<_>>>()?,
        ),
        serde_json::Value::Object(values) => Value::Dictionary(Dictionary::from_iter(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_plist(value)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
    })
}

fn merge_extension_attributes(extension: &mut Dictionary, attributes: Dictionary) {
    if !extension.contains_key("NSExtensionAttributes") {
        extension.insert(
            "NSExtensionAttributes".to_owned(),
            Value::Dictionary(Dictionary::new()),
        );
    }
    let existing_attributes = extension
        .get_mut("NSExtensionAttributes")
        .and_then(Value::as_dictionary_mut)
        .expect("NSExtensionAttributes must remain a dictionary");
    for (key, value) in attributes {
        existing_attributes.insert(key, value);
    }
}

fn parent_bundle_id(
    project: &ProjectContext,
    target_name: &str,
    parent_kind: TargetKind,
) -> Option<String> {
    project
        .manifest
        .targets
        .iter()
        .find(|candidate| {
            candidate.kind == parent_kind
                && candidate
                    .dependencies
                    .iter()
                    .any(|name| name == target_name)
        })
        .map(|target| target.bundle_id.clone())
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
        discover_resources(
            &resource_path,
            &resource_path,
            &mut asset_catalogs,
            &mut copy_jobs,
        )?;
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
    command
        .arg("--platform")
        .arg(toolchain.actool_platform_name());
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
    _project: &ProjectContext,
    root_target: &TargetManifest,
    built_targets: &HashMap<String, BuiltTarget>,
    built_root_target: &mut BuiltTarget,
) -> Result<()> {
    for dependency_name in &root_target.dependencies {
        let built = built_targets
            .get(dependency_name)
            .with_context(|| format!("missing built dependency `{dependency_name}`"))?;
        let Some(destination_root) = embedded_dependency_root(root_target.kind, built.target_kind)
        else {
            continue;
        };
        let destination = built_root_target.bundle_path.join(destination_root).join(
            built
                .bundle_path
                .file_name()
                .context("dependency bundle name missing")?,
        );
        copy_dir_recursive(&built.bundle_path, &destination)?;
    }
    Ok(())
}

fn embedded_dependency_root(
    parent_kind: TargetKind,
    child_kind: TargetKind,
) -> Option<&'static str> {
    match (parent_kind, child_kind) {
        (
            TargetKind::App | TargetKind::WatchApp,
            TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension,
        ) => Some("PlugIns"),
        (TargetKind::App, TargetKind::WatchApp) => Some("Watch"),
        (
            TargetKind::App
            | TargetKind::AppExtension
            | TargetKind::WatchApp
            | TargetKind::WatchExtension
            | TargetKind::WidgetExtension,
            TargetKind::Framework,
        ) => Some("Frameworks"),
        _ => None,
    }
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
            let artifact_name = explicit_output.map(Path::to_path_buf).unwrap_or_else(|| {
                project.project_paths.artifacts_dir.join(format!(
                    "{}-{:?}.ipa",
                    built_target.target_name, profile.distribution
                ))
            });
            let artifact_path = resolve_path(&project.root, &artifact_name);
            if artifact_path.exists() {
                fs::remove_file(&artifact_path).with_context(|| {
                    format!(
                        "failed to remove existing artifact {}",
                        artifact_path.display()
                    )
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
    if !device.is_booted() {
        let mut boot = Command::new("xcrun");
        boot.args(["simctl", "boot", &device.udid]);
        run_command(&mut boot)?;
    }

    let mut bootstatus = Command::new("xcrun");
    bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
    run_command(&mut bootstatus)?;

    let mut open_simulator = Command::new("open");
    open_simulator.args([
        "-a",
        "Simulator",
        "--args",
        "-CurrentDeviceUDID",
        &device.udid,
    ]);
    run_command(&mut open_simulator)?;

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

    println!(
        "Launching {} on {}. Orbit will stay attached to the simulator console; press Ctrl-C to stop.",
        receipt.bundle_id, device.name
    );

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
    let output = command_output(Command::new("xcrun").args([
        "simctl",
        "list",
        "devices",
        "available",
        "--json",
    ]))?;
    let devices: SimctlList = serde_json::from_str(&output)?;
    let mut flattened = devices.devices.into_values().flatten().collect::<Vec<_>>();
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
    let auth = crate::apple::auth::resolve_submit_auth(project)?;
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

#[derive(Debug, Clone, Deserialize)]
struct XcframeworkInfoPlist {
    #[serde(rename = "AvailableLibraries")]
    available_libraries: Vec<XcframeworkLibrary>,
}

#[derive(Debug, Clone, Deserialize)]
struct XcframeworkLibrary {
    #[serde(rename = "LibraryIdentifier")]
    library_identifier: String,
    #[serde(rename = "LibraryPath")]
    library_path: String,
    #[serde(rename = "HeadersPath")]
    headers_path: Option<String>,
    #[serde(rename = "SupportedPlatform")]
    supported_platform: String,
    #[serde(rename = "SupportedPlatformVariant")]
    supported_platform_variant: Option<String>,
    #[serde(rename = "SupportedArchitectures")]
    supported_architectures: Vec<String>,
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

fn resolve_external_link_inputs(
    project: &ProjectContext,
    toolchain: &Toolchain,
    intermediates_dir: &Path,
    target: &TargetManifest,
) -> Result<ExternalLinkInputs> {
    let mut inputs = ExternalLinkInputs {
        link_frameworks: target.frameworks.clone(),
        weak_frameworks: target.weak_frameworks.clone(),
        link_libraries: target.system_libraries.clone(),
        ..ExternalLinkInputs::default()
    };

    for dependency in &target.xcframeworks {
        merge_external_link_inputs(
            &mut inputs,
            resolve_xcframework_dependency(project, toolchain, intermediates_dir, dependency)?,
        );
    }

    dedup_vec(&mut inputs.module_search_paths);
    dedup_vec(&mut inputs.framework_search_paths);
    dedup_vec(&mut inputs.library_search_paths);
    dedup_vec(&mut inputs.link_frameworks);
    dedup_vec(&mut inputs.weak_frameworks);
    dedup_vec(&mut inputs.link_libraries);
    dedup_vec(&mut inputs.embedded_payloads);

    Ok(inputs)
}

fn resolve_xcframework_dependency(
    project: &ProjectContext,
    toolchain: &Toolchain,
    _intermediates_dir: &Path,
    dependency: &XcframeworkDependency,
) -> Result<ExternalLinkInputs> {
    let xcframework_root = resolve_path(&project.root, &dependency.path);
    let info_path = xcframework_root.join("Info.plist");
    let info: XcframeworkInfoPlist = plist::from_file(&info_path)
        .with_context(|| format!("failed to parse {}", info_path.display()))?;
    let library =
        select_xcframework_library(toolchain, &info.available_libraries).with_context(|| {
            format!(
                "failed to select XCFramework slice for {}",
                xcframework_root.display()
            )
        })?;
    let slice_root = xcframework_root.join(&library.library_identifier);
    let library_path = slice_root.join(&library.library_path);
    let mut inputs = ExternalLinkInputs::default();

    if let Some(headers_path) = &library.headers_path {
        let headers_root = slice_root.join(headers_path);
        inputs.module_search_paths.push(headers_root);
    }

    let explicit_name = dependency.library.as_ref().map(|name| name.as_str());
    let file_name = library_path
        .file_name()
        .and_then(OsStr::to_str)
        .context("XCFramework library path is missing a file name")?;
    if file_name.ends_with(".framework") {
        let framework_name = explicit_name
            .map(ToOwned::to_owned)
            .or_else(|| {
                Path::new(file_name)
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .map(ToOwned::to_owned)
            })
            .context("failed to derive XCFramework framework name")?;
        inputs.framework_search_paths.push(
            library_path
                .parent()
                .context("framework path is missing a parent")?
                .to_path_buf(),
        );
        inputs.link_frameworks.push(framework_name);
        if dependency.embed {
            inputs.embedded_payloads.push(library_path);
        }
    } else {
        let library_name = explicit_name
            .map(ToOwned::to_owned)
            .or_else(|| {
                file_name
                    .strip_prefix("lib")
                    .and_then(|value| {
                        value
                            .strip_suffix(".a")
                            .or_else(|| value.strip_suffix(".dylib"))
                    })
                    .map(ToOwned::to_owned)
            })
            .context("failed to derive XCFramework library name")?;
        inputs.library_search_paths.push(
            library_path
                .parent()
                .context("library path is missing a parent")?
                .to_path_buf(),
        );
        inputs.link_libraries.push(library_name);
        if dependency.embed && file_name.ends_with(".dylib") {
            inputs.embedded_payloads.push(library_path);
        }
    }

    Ok(inputs)
}

fn select_xcframework_library<'a>(
    toolchain: &Toolchain,
    available_libraries: &'a [XcframeworkLibrary],
) -> Option<&'a XcframeworkLibrary> {
    let platform = match toolchain.platform {
        ApplePlatform::Ios => "ios",
        ApplePlatform::Macos => "macos",
        ApplePlatform::Tvos => "tvos",
        ApplePlatform::Visionos => "xros",
        ApplePlatform::Watchos => "watchos",
    };
    let variant = match toolchain.destination {
        DestinationKind::Simulator => Some("simulator"),
        DestinationKind::Device => None,
    };

    available_libraries.iter().find(|library| {
        library.supported_platform == platform
            && library.supported_platform_variant.as_deref() == variant
            && library
                .supported_architectures
                .iter()
                .any(|architecture| architecture == &toolchain.architecture)
    })
}

fn merge_external_link_inputs(target: &mut ExternalLinkInputs, source: ExternalLinkInputs) {
    target
        .module_search_paths
        .extend(source.module_search_paths);
    target
        .framework_search_paths
        .extend(source.framework_search_paths);
    target
        .library_search_paths
        .extend(source.library_search_paths);
    target.link_frameworks.extend(source.link_frameworks);
    target.weak_frameworks.extend(source.weak_frameworks);
    target.link_libraries.extend(source.link_libraries);
    target.embedded_payloads.extend(source.embedded_payloads);
}

fn apply_external_link_inputs(command: &mut Command, inputs: &ExternalLinkInputs) {
    for path in &inputs.module_search_paths {
        command.arg("-I").arg(path);
    }
    for path in &inputs.framework_search_paths {
        command.arg("-F").arg(path);
    }
    for path in &inputs.library_search_paths {
        command.arg("-L").arg(path);
    }
    for framework in &inputs.link_frameworks {
        command.arg("-framework").arg(framework);
    }
    for framework in &inputs.weak_frameworks {
        command.arg("-weak_framework").arg(framework);
    }
    for library in &inputs.link_libraries {
        command.arg("-l").arg(library);
    }
}

fn embed_external_payloads(inputs: &ExternalLinkInputs, bundle_root: &Path) -> Result<()> {
    if inputs.embedded_payloads.is_empty() {
        return Ok(());
    }

    let frameworks_root = bundle_root.join("Frameworks");
    ensure_dir(&frameworks_root)?;
    for payload in &inputs.embedded_payloads {
        let file_name = payload
            .file_name()
            .context("embedded payload path is missing a file name")?;
        let destination = frameworks_root.join(file_name);
        if payload.is_dir() {
            copy_dir_recursive(payload, &destination)?;
        } else {
            copy_file(payload, &destination)?;
        }
    }
    Ok(())
}

fn dedup_vec<T>(values: &mut Vec<T>)
where
    T: Ord,
{
    values.sort();
    values.dedup();
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

    let module_dir = intermediates_dir
        .join("swiftmodules")
        .join(&dependency.product);
    let library_dir = intermediates_dir
        .join("swiftlibs")
        .join(&dependency.product);
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

impl SimulatorDevice {
    fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use plist::Dictionary;
    use serde_json::json;

    use super::{
        ApplePlatform, DestinationKind, ExtensionManifest, TargetKind, Toolchain,
        XcframeworkLibrary, embedded_dependency_root, extension_plist, json_to_plist,
        merge_extension_attributes, select_xcframework_library,
    };

    #[test]
    fn embeds_watch_children_into_expected_subdirectories() {
        assert_eq!(
            embedded_dependency_root(TargetKind::App, TargetKind::WatchApp),
            Some("Watch")
        );
        assert_eq!(
            embedded_dependency_root(TargetKind::WatchApp, TargetKind::WatchExtension),
            Some("PlugIns")
        );
        assert_eq!(
            embedded_dependency_root(TargetKind::WatchApp, TargetKind::Framework),
            Some("Frameworks")
        );
    }

    #[test]
    fn preserves_extra_extension_entries() {
        let extension = ExtensionManifest {
            point_identifier: "com.apple.widgetkit-extension".to_owned(),
            principal_class: "WidgetPrincipal".to_owned(),
            extra: BTreeMap::from([(
                "NSExtensionAttributes".to_owned(),
                json!({
                    "WKBackgroundModes": ["workout-processing"]
                }),
            )]),
        };
        let mut plist = extension_plist(&extension).unwrap();
        merge_extension_attributes(
            &mut plist,
            Dictionary::from_iter([(
                "WKAppBundleIdentifier".to_owned(),
                plist::Value::String("dev.orbit.examples.watch.watchkitapp".to_owned()),
            )]),
        );

        let attributes = plist
            .get("NSExtensionAttributes")
            .and_then(plist::Value::as_dictionary)
            .unwrap();
        assert_eq!(
            attributes
                .get("WKBackgroundModes")
                .and_then(plist::Value::as_array)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            attributes
                .get("WKAppBundleIdentifier")
                .and_then(plist::Value::as_string)
                .unwrap(),
            "dev.orbit.examples.watch.watchkitapp"
        );
    }

    #[test]
    fn converts_nested_json_values_into_plist_values() {
        let value = json_to_plist(&json!({
            "Enabled": true,
            "Count": 3,
            "Items": ["one", "two"]
        }))
        .unwrap();
        let dictionary = value.as_dictionary().unwrap();
        assert_eq!(
            dictionary
                .get("Enabled")
                .and_then(plist::Value::as_boolean)
                .unwrap(),
            true
        );
        assert_eq!(
            dictionary
                .get("Items")
                .and_then(plist::Value::as_array)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn selects_matching_xcframework_slice_for_target_platform() {
        let toolchain = Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Simulator,
            sdk_name: "iphonesimulator".to_owned(),
            sdk_path: "/tmp/sdk".into(),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
        };
        let slices = vec![
            XcframeworkLibrary {
                library_identifier: "ios-arm64".to_owned(),
                library_path: "Orbit.framework".to_owned(),
                headers_path: None,
                supported_platform: "ios".to_owned(),
                supported_platform_variant: None,
                supported_architectures: vec!["arm64".to_owned()],
            },
            XcframeworkLibrary {
                library_identifier: "ios-arm64_x86_64-simulator".to_owned(),
                library_path: "Orbit.framework".to_owned(),
                headers_path: None,
                supported_platform: "ios".to_owned(),
                supported_platform_variant: Some("simulator".to_owned()),
                supported_architectures: vec!["arm64".to_owned(), "x86_64".to_owned()],
            },
        ];

        let selected = select_xcframework_library(&toolchain, &slices).unwrap();
        assert_eq!(selected.library_identifier, "ios-arm64_x86_64-simulator");
    }
}
