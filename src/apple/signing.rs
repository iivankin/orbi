use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::apple::auth::{
    EnsureUserAuthRequest, ensure_portal_authenticated, resolve_user_auth_metadata,
};
use crate::apple::capabilities::{
    CapabilityRelationships, CapabilityUpdate, capability_sync_plan_from_entitlements,
};
use crate::apple::portal::{
    PortalAppId, PortalClient, PortalDeviceClass, PortalProfilePlatform, PortalProvisioningProfile,
};
use crate::apple::provisioning::{
    ProvisioningCapabilityRelationships, ProvisioningCapabilityUpdate, ProvisioningClient,
};
use crate::cli::{SigningExportArgs, SigningImportArgs, SigningSyncArgs, TargetPlatform};
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};
use crate::util::{
    copy_file, ensure_dir, prompt_multi_select, prompt_password, prompt_select,
    read_json_file_if_exists, write_json_file,
};

const P12_PASSWORD_SERVICE: &str = "dev.orbit.cli.codesign-p12";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SigningState {
    certificates: Vec<ManagedCertificate>,
    profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum CertificateOrigin {
    #[default]
    Generated,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedCertificate {
    id: String,
    certificate_type: String,
    serial_number: String,
    #[serde(default)]
    origin: CertificateOrigin,
    display_name: Option<String>,
    private_key_path: PathBuf,
    certificate_der_path: PathBuf,
    p12_path: PathBuf,
    p12_password_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedProfile {
    id: String,
    profile_type: String,
    bundle_id: String,
    path: PathBuf,
    uuid: Option<String>,
    certificate_ids: Vec<String>,
    device_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SigningMaterial {
    pub signing_identity: String,
    pub keychain_path: PathBuf,
    pub provisioning_profile_path: PathBuf,
    pub entitlements_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PackageSigningMaterial {
    pub signing_identity: String,
    pub keychain_path: PathBuf,
}

#[derive(Debug, Clone)]
struct TeamSigningPaths {
    state_path: PathBuf,
    certificates_dir: PathBuf,
    profiles_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct LocalSigningCleanSummary {
    pub removed_profiles: usize,
    pub removed_certificates: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteSigningCleanSummary {
    pub removed_apps: usize,
    pub removed_profiles: usize,
    pub removed_app_groups: usize,
    pub removed_merchants: usize,
    pub removed_certificates: usize,
    pub skipped_cloud_containers: Vec<String>,
}

pub fn sync_signing(project: &ProjectContext, args: &SigningSyncArgs) -> Result<()> {
    let target = resolve_signing_target(project, args.target.as_deref())?;
    let platform = project.manifest.resolve_platform_for_target(target, None)?;
    let profile_name = resolve_profile_name(project, platform, args.profile.as_deref())?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;

    if !args.device && args.simulator {
        println!("simulator builds do not require signing");
        return Ok(());
    }

    let device_udids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        Some(select_device_udids(
            project,
            profile.distribution,
            platform,
        )?)
    } else {
        None
    };

    let material = prepare_signing(project, target, platform, profile, device_udids)?;
    println!("identity: {}", material.signing_identity);
    println!("keychain: {}", material.keychain_path.display());
    println!(
        "provisioning_profile: {}",
        material.provisioning_profile_path.display()
    );
    if let Some(entitlements_path) = &material.entitlements_path {
        println!("entitlements: {}", entitlements_path.display());
    }
    Ok(())
}

pub fn export_signing_credentials(
    project: &ProjectContext,
    args: &SigningExportArgs,
) -> Result<()> {
    let selection = resolve_signing_selection(
        project,
        args.target.as_deref(),
        args.profile.as_deref(),
        args.platform.map(apple_platform_from_cli),
    )?;
    let team_id = resolve_local_team_id(project)?;
    let state = load_state(project, &team_id)?;
    let profile_type = profile_type(selection.platform, selection.profile)?;
    let managed_profile = state
        .profiles
        .iter()
        .rev()
        .find(|candidate| {
            candidate.bundle_id == selection.target.bundle_id
                && candidate.profile_type == profile_type
                && candidate.path.exists()
        })
        .with_context(|| {
            format!(
                "no Orbit-managed provisioning profile was found for `{}` ({}/{}) under Apple team `{team_id}`; run `orbit apple signing sync` first",
                selection.target.name, selection.platform, selection.profile_name
            )
        })?;
    let certificate_id = managed_profile
        .certificate_ids
        .first()
        .context("managed provisioning profile did not reference a signing certificate")?;
    let certificate = state
        .certificates
        .iter()
        .find(|candidate| candidate.id == *certificate_id && candidate.p12_path.exists())
        .with_context(|| {
            format!(
                "no local P12 was found for certificate `{certificate_id}` under Apple team `{team_id}`"
            )
        })?;

    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        project
            .project_paths
            .artifacts_dir
            .join("signing-export")
            .join(format!(
                "{}-{}-{}",
                selection.target.name, selection.platform, selection.profile_name
            ))
    });
    ensure_dir(&output_dir)?;

    let file_stem = format!(
        "{}-{}-{}",
        selection.target.name, selection.platform, selection.profile_name
    );
    let p12_path = output_dir.join(format!("{file_stem}.p12"));
    let profile_path = output_dir.join(format!("{file_stem}.mobileprovision"));
    copy_file(&certificate.p12_path, &p12_path)?;
    copy_file(&managed_profile.path, &profile_path)?;
    let password = load_p12_password(&certificate.p12_password_account)?;

    println!("team_id: {team_id}");
    println!("certificate_type: {}", certificate.certificate_type);
    println!("certificate_serial: {}", certificate.serial_number);
    println!("p12: {}", p12_path.display());
    println!("p12_password: {password}");
    println!("provisioning_profile: {}", profile_path.display());
    Ok(())
}

pub fn import_signing_credentials(
    project: &ProjectContext,
    args: &SigningImportArgs,
) -> Result<()> {
    let selection = resolve_signing_selection(
        project,
        args.target.as_deref(),
        args.profile.as_deref(),
        args.platform.map(apple_platform_from_cli),
    )?;
    let password = match &args.password {
        Some(password) => password.clone(),
        None if project.app.interactive => prompt_password("P12 password")?,
        None => bail!("--password is required in non-interactive mode"),
    };
    let team_id = resolve_local_team_id(project)?;
    let mut state = load_state(project, &team_id)?;
    let certificate_type = certificate_type(selection.platform, selection.profile)?;
    let paths = team_signing_paths(project, &team_id);
    ensure_dir(&paths.certificates_dir)?;

    let slug = format!("{}-{}", crate::util::timestamp_slug(), uuid::Uuid::new_v4());
    let imported_p12_path = paths.certificates_dir.join(format!("{slug}.p12"));
    let private_key_path = paths.certificates_dir.join(format!("{slug}.key.pem"));
    let certificate_pem_path = paths.certificates_dir.join(format!("{slug}.cert.pem"));
    let certificate_der_path = paths.certificates_dir.join(format!("{slug}.cer"));
    copy_file(&args.p12, &imported_p12_path)?;

    extract_private_key_from_p12(&imported_p12_path, &private_key_path, &password)?;
    extract_certificate_from_p12(&imported_p12_path, &certificate_pem_path, &password)?;
    export_certificate_der(&certificate_pem_path, &certificate_der_path)?;

    let serial_number = read_certificate_serial(&certificate_der_path)?;
    let display_name = read_certificate_common_name(&certificate_pem_path)?;
    let _ = fs::remove_file(&certificate_pem_path);
    let password_account = format!("imported-{serial_number}");
    store_p12_password(&password_account, &password)?;

    if let Some(existing_index) = state.certificates.iter().position(|candidate| {
        candidate.certificate_type == certificate_type
            && candidate.serial_number.eq_ignore_ascii_case(&serial_number)
    }) {
        let existing = state.certificates.remove(existing_index);
        delete_certificate_files(&existing)?;
        let _ = delete_p12_password(&existing.p12_password_account);
    }

    state.certificates.push(ManagedCertificate {
        id: format!("imported:{serial_number}"),
        certificate_type: certificate_type.to_owned(),
        serial_number: serial_number.clone(),
        origin: CertificateOrigin::Imported,
        display_name,
        private_key_path,
        certificate_der_path,
        p12_path: imported_p12_path,
        p12_password_account: password_account,
    });
    save_state(project, &team_id, &state)?;

    println!("team_id: {team_id}");
    println!("certificate_type: {certificate_type}");
    println!("certificate_serial: {serial_number}");
    println!("p12: {}", args.p12.display());
    Ok(())
}

struct SigningSelection<'a> {
    target: &'a TargetManifest,
    platform: ApplePlatform,
    profile_name: String,
    profile: &'a ProfileManifest,
}

fn resolve_signing_selection<'a>(
    project: &'a ProjectContext,
    requested_target: Option<&str>,
    requested_profile: Option<&str>,
    requested_platform: Option<ApplePlatform>,
) -> Result<SigningSelection<'a>> {
    let target = resolve_signing_target(project, requested_target)?;
    let platform = project
        .manifest
        .resolve_platform_for_target(target, requested_platform)?;
    let profile_name = resolve_profile_name(project, platform, requested_profile)?;
    let profile = project.manifest.profile_for(platform, &profile_name)?;
    Ok(SigningSelection {
        target,
        platform,
        profile_name,
        profile,
    })
}

fn apple_platform_from_cli(platform: TargetPlatform) -> ApplePlatform {
    match platform {
        TargetPlatform::Ios => ApplePlatform::Ios,
        TargetPlatform::Macos => ApplePlatform::Macos,
        TargetPlatform::Tvos => ApplePlatform::Tvos,
        TargetPlatform::Visionos => ApplePlatform::Visionos,
        TargetPlatform::Watchos => ApplePlatform::Watchos,
    }
}

fn resolve_signing_target<'a>(
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
    let index = prompt_select("Select a target to sync signing for", &labels)?;
    Ok(candidates.remove(index))
}

fn resolve_profile_name(
    project: &ProjectContext,
    platform: ApplePlatform,
    requested_profile: Option<&str>,
) -> Result<String> {
    if let Some(requested_profile) = requested_profile {
        let _ = project.manifest.profile_for(platform, requested_profile)?;
        return Ok(requested_profile.to_owned());
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

    let index = prompt_select("Select a signing profile", &profiles)?;
    Ok(profiles[index].clone())
}

pub fn prepare_signing(
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    profile: &ProfileManifest,
    device_udids: Option<Vec<String>>,
) -> Result<SigningMaterial> {
    let auth = ensure_portal_authenticated(
        &project.app,
        EnsureUserAuthRequest {
            team_id: project.manifest.team_id.clone(),
            provider_id: project.manifest.provider_id.clone(),
            prompt_for_missing: project.app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "signing requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    let mut client = PortalClient::from_session(&auth.session, team_id.clone())?;
    let provisioning = ProvisioningClient::from_session(&auth.session, team_id)?;
    let mut state = load_state(project, client.team_id())?;

    let bundle_id = ensure_bundle_id(&mut client, project, target, platform)?;
    sync_capabilities(
        &mut client,
        &provisioning,
        project,
        target,
        platform,
        &bundle_id,
    )?;

    let certificate_type = certificate_type(platform, profile)?;
    let certificate = ensure_certificate(&mut client, project, &mut state, certificate_type)?;
    let profile_type = profile_type(platform, profile)?;
    let device_ids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        let selected_udids = device_udids.unwrap_or_default();
        resolve_device_ids(&mut client, platform, &selected_udids)?
    } else {
        Vec::new()
    };
    let provisioning_profile = ensure_profile(
        &mut client,
        project,
        &mut state,
        platform,
        &bundle_id,
        profile_type,
        &certificate,
        &device_ids,
    )?;

    let signing_identity = import_certificate_into_keychain(project, &certificate)?;
    save_state(project, client.team_id(), &state)?;

    Ok(SigningMaterial {
        signing_identity,
        keychain_path: project.app.global_paths.keychain_path.clone(),
        provisioning_profile_path: provisioning_profile.path,
        entitlements_path: target
            .entitlements
            .as_ref()
            .map(|path| project.root.join(path)),
    })
}

pub fn prepare_package_signing(
    project: &ProjectContext,
    profile: &ProfileManifest,
) -> Result<PackageSigningMaterial> {
    let auth = ensure_portal_authenticated(
        &project.app,
        EnsureUserAuthRequest {
            team_id: project.manifest.team_id.clone(),
            provider_id: project.manifest.provider_id.clone(),
            prompt_for_missing: project.app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "installer signing requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    let mut client = PortalClient::from_session(&auth.session, team_id)?;
    let mut state = load_state(project, client.team_id())?;
    let certificate_type = installer_certificate_type(profile)?;
    let certificate = ensure_certificate(&mut client, project, &mut state, certificate_type)?;
    let signing_identity = import_certificate_into_keychain(project, &certificate)?;
    save_state(project, client.team_id(), &state)?;
    Ok(PackageSigningMaterial {
        signing_identity,
        keychain_path: project.app.global_paths.keychain_path.clone(),
    })
}

pub fn sign_bundle(bundle_path: &Path, material: &SigningMaterial) -> Result<()> {
    let embedded_profile = bundle_path.join("embedded.mobileprovision");
    fs::copy(&material.provisioning_profile_path, &embedded_profile).with_context(|| {
        format!(
            "failed to embed provisioning profile into {}",
            bundle_path.display()
        )
    })?;

    let mut command = Command::new("codesign");
    command.args(["--force", "--sign"]);
    command.arg(&material.signing_identity);
    command.args(["--keychain"]);
    command.arg(&material.keychain_path);
    if let Some(entitlements) = &material.entitlements_path {
        command.args(["--entitlements"]);
        command.arg(entitlements);
    }
    command.arg(bundle_path);
    crate::util::run_command(&mut command)
}

fn ensure_bundle_id(
    client: &mut PortalClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
) -> Result<PortalAppId> {
    if let Some(bundle_id) =
        client.find_app_by_bundle_id(&target.bundle_id, matches!(platform, ApplePlatform::Macos))?
    {
        return Ok(bundle_id);
    }

    client.create_app(
        &format!("@orbit/{}", project.manifest.name),
        &target.bundle_id,
        matches!(platform, ApplePlatform::Macos),
    )
}

fn sync_capabilities(
    client: &mut PortalClient,
    provisioning: &ProvisioningClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
    bundle_id: &PortalAppId,
) -> Result<()> {
    let Some(entitlements_path) = &target.entitlements else {
        return Ok(());
    };
    let provisioning_bundle = provisioning
        .find_bundle_id(&bundle_id.identifier)?
        .with_context(|| {
            format!(
                "bundle identifier `{}` exists in the Developer Portal but could not be loaded from Apple's provisioning API",
                bundle_id.identifier
            )
        })?;
    let plan = capability_sync_plan_from_entitlements(
        &project.root.join(entitlements_path),
        &provisioning_bundle.capabilities,
    )?;
    if plan.updates.is_empty() {
        return Ok(());
    }

    let app_group_ids = resolve_app_group_ids(
        client,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }),
    )?;
    let merchant_ids = resolve_merchant_ids(
        client,
        platform,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.merchant_ids.as_ref()
        }),
    )?;
    let cloud_container_ids = resolve_cloud_container_ids(
        client,
        collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_ref()
        }),
    )?;
    let updates = plan
        .updates
        .iter()
        .map(|update| {
            Ok(ProvisioningCapabilityUpdate {
                capability_type: update.capability_type.clone(),
                option: update.option.clone(),
                relationships: ProvisioningCapabilityRelationships {
                    app_groups: map_relationship_ids(
                        update.relationships.app_groups.as_ref(),
                        &app_group_ids,
                    )?,
                    merchant_ids: map_relationship_ids(
                        update.relationships.merchant_ids.as_ref(),
                        &merchant_ids,
                    )?,
                    cloud_containers: map_relationship_ids(
                        update.relationships.cloud_containers.as_ref(),
                        &cloud_container_ids,
                    )?,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    provisioning.update_bundle_capabilities(&provisioning_bundle, &updates)?;
    Ok(())
}

fn collect_identifier_values<F>(updates: &[CapabilityUpdate], select: F) -> Vec<String>
where
    F: Fn(&CapabilityRelationships) -> Option<&Vec<String>>,
{
    let mut values = updates
        .iter()
        .flat_map(|update| {
            select(&update.relationships)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn resolve_app_group_ids(
    client: &mut PortalClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_app_groups()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_group) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_group.id.clone()
        } else {
            let name = identifier_name("App Group", &identifier);
            client.create_app_group(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_merchant_ids(
    client: &mut PortalClient,
    platform: ApplePlatform,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_merchants(platform == ApplePlatform::Macos)?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_merchant) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_merchant.id.clone()
        } else {
            let name = identifier_name("Merchant ID", &identifier);
            client
                .create_merchant(&name, &identifier, platform == ApplePlatform::Macos)?
                .id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn resolve_cloud_container_ids(
    client: &mut PortalClient,
    identifiers: Vec<String>,
) -> Result<HashMap<String, String>> {
    if identifiers.is_empty() {
        return Ok(HashMap::new());
    }

    let existing = client.list_cloud_containers()?;
    let mut resolved = HashMap::new();
    for identifier in identifiers {
        let id = if let Some(existing_container) = existing
            .iter()
            .find(|candidate| candidate.identifier == identifier)
        {
            existing_container.id.clone()
        } else {
            let name = identifier_name("iCloud Container", &identifier);
            client.create_cloud_container(&name, &identifier)?.id
        };
        resolved.insert(identifier, id);
    }
    Ok(resolved)
}

fn map_relationship_ids(
    identifiers: Option<&Vec<String>>,
    resolved: &HashMap<String, String>,
) -> Result<Option<Vec<String>>> {
    let Some(identifiers) = identifiers else {
        return Ok(None);
    };
    identifiers
        .iter()
        .map(|identifier| {
            resolved
                .get(identifier)
                .cloned()
                .with_context(|| format!("missing Apple identifier record for `{identifier}`"))
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn ensure_certificate(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
) -> Result<ManagedCertificate> {
    let remote_certificates = client.list_certificates(
        &[certificate_type],
        is_macos_certificate_type(certificate_type),
    )?;
    for remote in &remote_certificates {
        if let Some(local) = state.certificates.iter_mut().find(|certificate| {
            if certificate.certificate_type != certificate_type || !certificate.p12_path.exists() {
                return false;
            }
            if certificate.id == remote.id {
                return true;
            }
            remote
                .serial_number
                .as_deref()
                .is_some_and(|serial| certificate.serial_number.eq_ignore_ascii_case(serial))
        }) {
            if local.id != remote.id {
                local.id = remote.id.clone();
            }
            if local.display_name.is_none() {
                local.display_name = remote.name.clone();
            }
            return Ok(local.clone());
        }
    }

    let paths = team_signing_paths(project, client.team_id());
    ensure_dir(&paths.certificates_dir)?;
    let slug = crate::util::timestamp_slug();
    let private_key_path = paths.certificates_dir.join(format!("{slug}.key.pem"));
    let csr_path = paths.certificates_dir.join(format!("{slug}.csr.pem"));
    let certificate_der_path = paths.certificates_dir.join(format!("{slug}.cer"));
    let p12_path = paths.certificates_dir.join(format!("{slug}.p12"));

    let mut openssl_req = Command::new("openssl");
    openssl_req.args([
        "req",
        "-new",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-subj",
        &format!("/CN=Orbit {slug}"),
        "-out",
        csr_path
            .to_str()
            .context("CSR path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut openssl_req)?;

    let csr_pem = fs::read_to_string(&csr_path)
        .with_context(|| format!("failed to read {}", csr_path.display()))?;
    let remote = client.create_certificate(
        certificate_type,
        &csr_pem,
        is_macos_certificate_type(certificate_type),
    )?;
    let certificate_bytes = client.download_certificate(
        &remote.id,
        certificate_type,
        is_macos_certificate_type(certificate_type),
    )?;
    fs::write(&certificate_der_path, &certificate_bytes)
        .with_context(|| format!("failed to write {}", certificate_der_path.display()))?;

    let p12_password = uuid::Uuid::new_v4().to_string();
    let mut openssl_pkcs12 = Command::new("openssl");
    openssl_pkcs12.args([
        "pkcs12",
        "-export",
        "-inkey",
        private_key_path
            .to_str()
            .context("private key path contains invalid UTF-8")?,
        "-in",
        certificate_der_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-inform",
        "DER",
        "-out",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-passout",
        &format!("pass:{p12_password}"),
    ]);
    crate::util::run_command(&mut openssl_pkcs12)?;

    let serial_number = match remote.serial_number.clone() {
        Some(serial_number) => serial_number,
        None => read_certificate_serial(&certificate_der_path)?,
    };
    let password_account = format!("{}-{serial_number}", remote.id);
    store_p12_password(&password_account, &p12_password)?;

    let certificate = ManagedCertificate {
        id: remote.id,
        certificate_type: certificate_type.to_owned(),
        serial_number,
        origin: CertificateOrigin::Generated,
        display_name: remote.name.clone(),
        private_key_path,
        certificate_der_path,
        p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(certificate)
}

fn ensure_profile(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    platform: ApplePlatform,
    bundle_id: &PortalAppId,
    profile_type: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
) -> Result<ManagedProfile> {
    let portal_platform = portal_profile_platform(platform);
    let profiles = client.list_profiles(portal_platform)?;
    let bundle_identifier = &bundle_id.identifier;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();

    for profile in profiles {
        remote_profile_ids.insert(profile.id.clone());
        let Some(app) = &profile.app else {
            continue;
        };
        if app.id != bundle_id.id {
            continue;
        }

        let certificate_links = profile
            .certificates
            .iter()
            .map(|certificate| certificate.id.clone())
            .collect::<Vec<_>>();
        let device_links = profile
            .devices
            .iter()
            .map(|device| device.id.clone())
            .collect::<Vec<_>>();

        let matches_certificate = certificate_links.contains(&certificate.id);
        let matches_devices = canonical_ids(&device_links) == canonical_ids(device_ids);

        if matches_certificate && matches_devices {
            let managed = persist_profile(
                client,
                project,
                state,
                portal_platform,
                profile_type,
                bundle_identifier,
                certificate,
                &device_links,
                profile,
            )?;
            cleanup_stale_profile_state(
                state,
                bundle_identifier,
                profile_type,
                &remote_profile_ids,
            );
            return Ok(managed);
        }

        if is_orbit_managed_profile(state, &profile.id, bundle_identifier, profile_type) {
            stale_orbit_profiles.push(profile.id.clone());
        }
    }

    for profile_id in stale_orbit_profiles {
        client
            .delete_profile(portal_platform, &profile_id)
            .with_context(|| format!("failed to repair provisioning profile `{profile_id}`"))?;
        state.profiles.retain(|profile| profile.id != profile_id);
    }
    cleanup_stale_profile_state(state, bundle_identifier, profile_type, &remote_profile_ids);

    let remote = client.create_profile(
        portal_platform,
        &format!(
            "*[orbit] {} {} {}",
            bundle_identifier,
            profile_type,
            crate::util::timestamp_slug()
        ),
        profile_type,
        &bundle_id.id,
        &[certificate.id.clone()],
        device_ids,
    )?;
    persist_profile(
        client,
        project,
        state,
        portal_platform,
        profile_type,
        bundle_identifier,
        certificate,
        device_ids,
        remote,
    )
}

fn persist_profile(
    client: &mut PortalClient,
    project: &ProjectContext,
    state: &mut SigningState,
    platform: PortalProfilePlatform,
    profile_type: &str,
    bundle_identifier: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
    remote: PortalProvisioningProfile,
) -> Result<ManagedProfile> {
    let paths = team_signing_paths(project, client.team_id());
    ensure_dir(&paths.profiles_dir)?;
    let profile_bytes = client.download_profile(platform, &remote.id)?;
    let profile_path = paths
        .profiles_dir
        .join(format!("{}-{}.mobileprovision", remote.id, profile_type));
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;

    state.profiles.retain(|profile| profile.id != remote.id);
    let profile = ManagedProfile {
        id: remote.id,
        profile_type: profile_type.to_owned(),
        bundle_id: bundle_identifier.to_owned(),
        path: profile_path,
        uuid: remote.uuid.clone(),
        certificate_ids: vec![certificate.id.clone()],
        device_ids: device_ids.to_vec(),
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_ids(
    client: &mut PortalClient,
    platform: ApplePlatform,
    udids: &[String],
) -> Result<Vec<String>> {
    let class = device_class_for_platform(platform);
    if udids.is_empty() {
        return Ok(client
            .list_devices(class, false)?
            .into_iter()
            .map(|device| device.id)
            .collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = client
            .find_device_by_udid(udid, class)?
            .with_context(|| format!("device `{udid}` is not registered with Apple"))?;
        device_ids.push(device.id);
    }
    Ok(device_ids)
}

fn select_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
    platform: ApplePlatform,
) -> Result<Vec<String>> {
    let cache = crate::apple::device::refresh_cache(&project.app)?;
    let devices = cache
        .devices
        .into_iter()
        .filter(|device| device_matches_platform(&device.platform, platform))
        .collect::<Vec<_>>();
    if devices.is_empty() {
        bail!(
            "no registered Apple devices found for {platform}; run `orbit apple device register` first"
        );
    }

    if !project.app.interactive {
        return Ok(devices.into_iter().map(|device| device.udid).collect());
    }

    let labels = devices
        .iter()
        .map(|device| format!("{} ({})", device.name, device.udid))
        .collect::<Vec<_>>();
    if matches!(distribution, DistributionKind::AdHoc) {
        let defaults = vec![true; labels.len()];
        let selections = prompt_multi_select(
            "Select devices to include in the ad-hoc provisioning profile",
            &labels,
            Some(&defaults),
        )?;
        if selections.is_empty() {
            bail!("select at least one device for an ad-hoc provisioning profile");
        }
        return Ok(selections
            .into_iter()
            .map(|index| devices[index].udid.clone())
            .collect());
    }

    let index = prompt_select("Select a device to provision", &labels)?;
    Ok(vec![devices[index].udid.clone()])
}

fn device_class_for_platform(platform: ApplePlatform) -> PortalDeviceClass {
    match platform {
        ApplePlatform::Ios | ApplePlatform::Visionos => PortalDeviceClass::Iphone,
        ApplePlatform::Tvos => PortalDeviceClass::Tvos,
        ApplePlatform::Watchos => PortalDeviceClass::Watch,
        ApplePlatform::Macos => PortalDeviceClass::Mac,
    }
}

fn device_matches_platform(device_platform: &str, platform: ApplePlatform) -> bool {
    match platform {
        ApplePlatform::Ios | ApplePlatform::Visionos => device_platform == "IOS",
        ApplePlatform::Tvos => device_platform == "TVOS",
        ApplePlatform::Watchos => device_platform == "WATCH" || device_platform == "WATCHOS",
        ApplePlatform::Macos => device_platform == "MAC_OS" || device_platform == "UNIVERSAL",
    }
}

fn portal_profile_platform(platform: ApplePlatform) -> PortalProfilePlatform {
    match platform {
        ApplePlatform::Ios => PortalProfilePlatform::Ios,
        ApplePlatform::Tvos => PortalProfilePlatform::Tvos,
        ApplePlatform::Watchos => PortalProfilePlatform::Watchos,
        ApplePlatform::Visionos => PortalProfilePlatform::Visionos,
        ApplePlatform::Macos => PortalProfilePlatform::Macos,
    }
}

fn is_orbit_managed_profile(
    state: &SigningState,
    profile_id: &str,
    bundle_identifier: &str,
    profile_type: &str,
) -> bool {
    state.profiles.iter().any(|profile| {
        profile.id == profile_id
            && profile.bundle_id == bundle_identifier
            && profile.profile_type == profile_type
    })
}

fn cleanup_stale_profile_state(
    state: &mut SigningState,
    bundle_identifier: &str,
    profile_type: &str,
    remote_profile_ids: &HashSet<String>,
) {
    state.profiles.retain(|profile| {
        if profile.bundle_id != bundle_identifier || profile.profile_type != profile_type {
            return true;
        }
        remote_profile_ids.contains(&profile.id)
    });
}

fn certificate_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("83Q87W3TGH"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc | DistributionKind::AppStore,
        ) => Ok("WXV89964HE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("749Y1QAGU7"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("HXZEUKP0FP"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("W0EURJRMC5"),
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn profile_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::Development,
        ) => Ok("limited"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AdHoc,
        ) => Ok("adhoc"),
        (
            ApplePlatform::Ios
            | ApplePlatform::Tvos
            | ApplePlatform::Visionos
            | ApplePlatform::Watchos,
            DistributionKind::AppStore,
        ) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("limited"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("store"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("direct"),
        _ => bail!("provisioning profiles are not implemented for {platform}"),
    }
}

fn installer_certificate_type(profile: &ProfileManifest) -> Result<&'static str> {
    match profile.distribution {
        DistributionKind::MacAppStore => Ok("2PQI8IDXNH"),
        DistributionKind::DeveloperId => Ok("OYVN2GW35E"),
        _ => bail!(
            "installer signing is not implemented for {:?}",
            profile.distribution
        ),
    }
}

fn identifier_name(prefix: &str, identifier: &str) -> String {
    format!("{prefix} {identifier}")
}

fn is_macos_certificate_type(certificate_type: &str) -> bool {
    matches!(
        certificate_type,
        "749Y1QAGU7" | "HXZEUKP0FP" | "2PQI8IDXNH" | "OYVN2GW35E" | "W0EURJRMC5"
    )
}

fn read_certificate_serial(path: &Path) -> Result<String> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-inform",
        "DER",
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-serial",
    ]);
    let output = crate::util::command_output(&mut command)?;
    output
        .trim()
        .strip_prefix("serial=")
        .map(ToOwned::to_owned)
        .context("openssl did not return a certificate serial number")
}

fn team_signing_paths(project: &ProjectContext, team_id: &str) -> TeamSigningPaths {
    let team_dir = project
        .app
        .global_paths
        .data_dir
        .join("teams")
        .join(team_id);
    TeamSigningPaths {
        state_path: team_dir.join("signing.json"),
        certificates_dir: team_dir.join("certificates"),
        profiles_dir: team_dir.join("profiles"),
    }
}

fn resolve_local_team_id_if_known(project: &ProjectContext) -> Result<Option<String>> {
    Ok(std::env::var("ORBIT_APPLE_TEAM_ID")
        .ok()
        .or_else(|| std::env::var("EXPO_APPLE_TEAM_ID").ok())
        .or_else(|| project.manifest.team_id.clone())
        .or_else(|| {
            resolve_user_auth_metadata(&project.app)
                .ok()
                .flatten()
                .and_then(|user| user.team_id)
        }))
}

fn resolve_local_team_id(project: &ProjectContext) -> Result<String> {
    resolve_local_team_id_if_known(project)?.context(
        "signing state is scoped by Apple team; set `team_id` in orbit.json, export ORBIT_APPLE_TEAM_ID, or log in once so Orbit can persist the team selection",
    )
}

fn load_state(project: &ProjectContext, team_id: &str) -> Result<SigningState> {
    let paths = team_signing_paths(project, team_id);
    Ok(read_json_file_if_exists(&paths.state_path)?.unwrap_or_default())
}

fn save_state(project: &ProjectContext, team_id: &str, state: &SigningState) -> Result<()> {
    let paths = team_signing_paths(project, team_id);
    write_json_file(&paths.state_path, state)
}

pub fn clean_local_signing_state(project: &ProjectContext) -> Result<LocalSigningCleanSummary> {
    let Some(team_id) = resolve_local_team_id_if_known(project)? else {
        return Ok(LocalSigningCleanSummary::default());
    };
    let mut state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let mut removed_profile_cert_ids = HashSet::new();
    let mut removed_profiles = 0usize;

    state.profiles.retain(|profile| {
        if !bundle_ids.contains(&profile.bundle_id) {
            return true;
        }
        let _ = delete_file_if_exists(&profile.path);
        removed_profile_cert_ids.extend(profile.certificate_ids.iter().cloned());
        removed_profiles += 1;
        false
    });

    let remaining_certificate_ids = state
        .profiles
        .iter()
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let mut removed_certificates = 0usize;
    state.certificates.retain(|certificate| {
        if !removed_profile_cert_ids.contains(&certificate.id)
            || remaining_certificate_ids.contains(&certificate.id)
        {
            return true;
        }
        let _ = delete_certificate_files(certificate);
        let _ = delete_p12_password(&certificate.p12_password_account);
        removed_certificates += 1;
        false
    });

    save_state(project, &team_id, &state)?;
    Ok(LocalSigningCleanSummary {
        removed_profiles,
        removed_certificates,
    })
}

pub fn clean_remote_signing_state(project: &ProjectContext) -> Result<RemoteSigningCleanSummary> {
    let auth = ensure_portal_authenticated(
        &project.app,
        EnsureUserAuthRequest {
            team_id: project.manifest.team_id.clone(),
            provider_id: project.manifest.provider_id.clone(),
            prompt_for_missing: project.app.interactive,
            ..Default::default()
        },
    )?;
    let team_id = auth.user.team_id.clone().context(
        "remote cleanup requires an Apple Developer team selection; log in again and choose a team if prompted",
    )?;
    let mut client = PortalClient::from_session(&auth.session, team_id.clone())?;
    let state = load_state(project, &team_id)?;
    let bundle_ids = project_bundle_ids(project);
    let orbit_app_name = format!("@orbit/{}", project.manifest.name);
    let mut summary = RemoteSigningCleanSummary::default();

    for platform in project.manifest.platforms.keys().copied() {
        for profile in client.list_profiles(portal_profile_platform(platform))? {
            let Some(app) = &profile.app else {
                continue;
            };
            if bundle_ids.contains(&app.identifier) && profile.name.starts_with("*[orbit] ") {
                client.delete_profile(portal_profile_platform(platform), &profile.id)?;
                summary.removed_profiles += 1;
            }
        }
    }

    for target in &project.manifest.targets {
        let platform = project.manifest.resolve_platform_for_target(target, None)?;
        let mac = matches!(platform, ApplePlatform::Macos);
        if let Some(app) = client.find_app_by_bundle_id(&target.bundle_id, mac)?
            && app.name == orbit_app_name
        {
            client.delete_app(&app.id, mac)?;
            summary.removed_apps += 1;
        }
    }

    let ProjectEntitlementIdentifiers {
        app_groups: entitlement_app_groups,
        merchant_ids_by_mac,
        cloud_containers,
    } = project_entitlement_identifiers(project)?;
    let app_groups = client.list_app_groups()?;
    for identifier in entitlement_app_groups {
        if let Some(group) = app_groups.iter().find(|group| {
            group.identifier == identifier
                && group.name == identifier_name("App Group", &identifier)
        }) {
            client.delete_app_group(&group.id)?;
            summary.removed_app_groups += 1;
        }
    }

    for (mac, identifiers) in merchant_ids_by_mac {
        let merchants = client.list_merchants(mac)?;
        for identifier in identifiers {
            if let Some(merchant) = merchants.iter().find(|merchant| {
                merchant.identifier == identifier
                    && merchant.name == identifier_name("Merchant ID", &identifier)
            }) {
                client.delete_merchant(&merchant.id, mac)?;
                summary.removed_merchants += 1;
            }
        }
    }

    summary.skipped_cloud_containers = cloud_containers;

    let project_certificate_ids = state
        .profiles
        .iter()
        .filter(|profile| bundle_ids.contains(&profile.bundle_id))
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let certificate_ids_used_elsewhere = state
        .profiles
        .iter()
        .filter(|profile| !bundle_ids.contains(&profile.bundle_id))
        .flat_map(|profile| profile.certificate_ids.iter().cloned())
        .collect::<HashSet<_>>();
    for certificate in &state.certificates {
        if !project_certificate_ids.contains(&certificate.id)
            || certificate_ids_used_elsewhere.contains(&certificate.id)
            || certificate.origin != CertificateOrigin::Generated
        {
            continue;
        }
        client.revoke_certificate(
            &certificate.id,
            &certificate.certificate_type,
            is_macos_certificate_type(&certificate.certificate_type),
        )?;
        summary.removed_certificates += 1;
    }

    Ok(summary)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProjectEntitlementIdentifiers {
    app_groups: Vec<String>,
    merchant_ids_by_mac: HashMap<bool, Vec<String>>,
    cloud_containers: Vec<String>,
}

fn project_bundle_ids(project: &ProjectContext) -> HashSet<String> {
    project
        .manifest
        .targets
        .iter()
        .map(|target| target.bundle_id.clone())
        .collect()
}

fn project_entitlement_identifiers(
    project: &ProjectContext,
) -> Result<ProjectEntitlementIdentifiers> {
    let mut app_groups = HashSet::new();
    let mut merchant_ids_by_mac = HashMap::<bool, HashSet<String>>::new();
    let mut cloud_containers = HashSet::new();

    for target in &project.manifest.targets {
        let Some(entitlements_path) = &target.entitlements else {
            continue;
        };
        let platform = project.manifest.resolve_platform_for_target(target, None)?;
        let plan =
            capability_sync_plan_from_entitlements(&project.root.join(entitlements_path), &[])?;
        app_groups.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.app_groups.as_ref()
        }));
        cloud_containers.extend(collect_identifier_values(&plan.updates, |relationships| {
            relationships.cloud_containers.as_ref()
        }));
        merchant_ids_by_mac
            .entry(matches!(platform, ApplePlatform::Macos))
            .or_default()
            .extend(collect_identifier_values(&plan.updates, |relationships| {
                relationships.merchant_ids.as_ref()
            }));
    }

    Ok(ProjectEntitlementIdentifiers {
        app_groups: sorted_strings(app_groups),
        merchant_ids_by_mac: merchant_ids_by_mac
            .into_iter()
            .map(|(mac, identifiers)| (mac, sorted_strings(identifiers)))
            .collect(),
        cloud_containers: sorted_strings(cloud_containers),
    })
}

fn sorted_strings(values: HashSet<String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn canonical_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids
}

fn store_p12_password(account: &str, password: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
        "-w",
        password,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

fn load_p12_password(account: &str) -> Result<String> {
    let mut command = Command::new("security");
    command.args([
        "find-generic-password",
        "-w",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|value| value.trim().to_owned())
}

fn delete_p12_password(account: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "delete-generic-password",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

fn extract_private_key_from_p12(p12_path: &Path, output_path: &Path, password: &str) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-nodes",
        "-nocerts",
        "-out",
        output_path
            .to_str()
            .context("private key output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    crate::util::run_command(&mut command)
}

fn extract_certificate_from_p12(p12_path: &Path, output_path: &Path, password: &str) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-clcerts",
        "-nokeys",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    crate::util::run_command(&mut command)
}

fn export_certificate_der(certificate_pem_path: &Path, output_path: &Path) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-in",
        certificate_pem_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-outform",
        "DER",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
    ]);
    crate::util::run_command(&mut command)
}

fn read_certificate_common_name(path: &Path) -> Result<Option<String>> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-subject",
    ]);
    let output = crate::util::command_output(&mut command)?;
    let subject = output.trim();
    let common_name = subject
        .split(',')
        .find_map(|segment| {
            let segment = segment.trim();
            segment
                .strip_prefix("subject=")
                .unwrap_or(segment)
                .trim()
                .strip_prefix("CN = ")
                .or_else(|| segment.trim().strip_prefix("CN="))
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.is_empty());
    Ok(common_name)
}

fn delete_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn delete_certificate_files(certificate: &ManagedCertificate) -> Result<()> {
    delete_file_if_exists(&certificate.private_key_path)?;
    delete_file_if_exists(&certificate.certificate_der_path)?;
    delete_file_if_exists(&certificate.p12_path)
}

fn import_certificate_into_keychain(
    project: &ProjectContext,
    certificate: &ManagedCertificate,
) -> Result<String> {
    let keychain_path = &project.app.global_paths.keychain_path;
    if !keychain_path.exists() {
        let mut create = Command::new("security");
        create.args([
            "create-keychain",
            "-p",
            "",
            keychain_path
                .to_str()
                .context("keychain path contains invalid UTF-8")?,
        ]);
        crate::util::run_command(&mut create)?;
    }

    let keychain_str = keychain_path
        .to_str()
        .context("keychain path contains invalid UTF-8")?;
    let mut unlock = Command::new("security");
    unlock.args(["unlock-keychain", "-p", "", keychain_str]);
    let _ = crate::util::run_command(&mut unlock);

    let mut settings = Command::new("security");
    settings.args(["set-keychain-settings", "-lut", "21600", keychain_str]);
    let _ = crate::util::run_command(&mut settings);

    let p12_password = load_p12_password(&certificate.p12_password_account)?;
    let mut import = Command::new("security");
    import.args([
        "import",
        certificate
            .p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-k",
        keychain_str,
        "-P",
        &p12_password,
        "-T",
        "/usr/bin/codesign",
        "-T",
        "/usr/bin/security",
    ]);
    let _ = crate::util::run_command(&mut import);

    let mut partition = Command::new("security");
    partition.args([
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:",
        "-s",
        "-k",
        "",
        keychain_str,
    ]);
    let _ = crate::util::run_command(&mut partition);

    let mut find_identity = Command::new("security");
    find_identity.args(["find-identity", "-v", "-p", "codesigning", keychain_str]);
    let output = crate::util::command_output(&mut find_identity)?;
    let serial = certificate.serial_number.to_lowercase();
    for line in output.lines() {
        if line.to_lowercase().contains(&serial) {
            if let Some(start) = line.find('"') {
                if let Some(end) = line[start + 1..].find('"') {
                    return Ok(line[start + 1..start + 1 + end].to_owned());
                }
            }
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() >= 2 {
                return Ok(parts[1].trim_matches('"').to_owned());
            }
        }
    }

    if let Some(display_name) = &certificate.display_name {
        return Ok(display_name.clone());
    }
    bail!(
        "failed to resolve imported signing identity for certificate {}",
        certificate.id
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use plist::Value;
    use tempfile::TempDir;

    use super::{
        CertificateOrigin, ManagedCertificate, ManagedProfile, ProfileManifest,
        ProjectEntitlementIdentifiers, SigningState, clean_local_signing_state, load_state,
        project_entitlement_identifiers, save_state, team_signing_paths,
    };
    use crate::context::{AppContext, GlobalPaths, ProjectContext, ProjectPaths};
    use crate::manifest::{
        ApplePlatform, DistributionKind, Manifest, PlatformManifest, TargetKind, TargetManifest,
        ToolchainManifest,
    };

    fn test_project() -> (TempDir, ProjectContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        let orbit_dir = root.join(".orbit");
        let build_dir = orbit_dir.join("build");
        let artifacts_dir = orbit_dir.join("artifacts");
        let receipts_dir = orbit_dir.join("receipts");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::create_dir_all(&receipts_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manifest = Manifest {
            name: "OrbitFixture".to_owned(),
            version: "0.1.0".to_owned(),
            platform: "apple".to_owned(),
            team_id: Some("TEAM123456".to_owned()),
            provider_id: None,
            source_roots: Vec::new(),
            toolchain: ToolchainManifest::default(),
            platforms: BTreeMap::from([(
                ApplePlatform::Ios,
                PlatformManifest {
                    deployment_target: "18.0".to_owned(),
                    profiles: BTreeMap::from([(
                        "development".to_owned(),
                        ProfileManifest {
                            configuration: "debug".to_owned(),
                            distribution: DistributionKind::Development,
                        },
                    )]),
                },
            )]),
            targets: vec![TargetManifest {
                name: "ExampleApp".to_owned(),
                kind: TargetKind::App,
                bundle_id: "dev.orbit.fixture".to_owned(),
                platforms: vec![ApplePlatform::Ios],
                sources: vec![root.join("Sources/App")],
                resources: Vec::new(),
                dependencies: Vec::new(),
                frameworks: Vec::new(),
                weak_frameworks: Vec::new(),
                system_libraries: Vec::new(),
                xcframeworks: Vec::new(),
                swift_packages: Vec::new(),
                entitlements: None,
                extension: None,
            }],
        };
        let manifest_path = root.join("orbit.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let app = AppContext {
            cwd: root.clone(),
            interactive: false,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                auth_state_path: data_dir.join("auth.json"),
                device_cache_path: data_dir.join("devices.json"),
                keychain_path: data_dir.join("orbit.keychain-db"),
            },
        };
        let project = ProjectContext {
            app,
            root: root.clone(),
            manifest_path,
            manifest,
            project_paths: ProjectPaths {
                orbit_dir,
                build_dir,
                artifacts_dir,
                receipts_dir,
            },
        };
        (temp, project)
    }

    #[test]
    fn local_cleanup_removes_only_current_project_profiles_and_unused_certs() {
        let (_temp, project) = test_project();
        let team_id = "TEAM123456";
        let team_paths = team_signing_paths(&project, team_id);
        std::fs::create_dir_all(&team_paths.certificates_dir).unwrap();
        std::fs::create_dir_all(&team_paths.profiles_dir).unwrap();

        let current_profile_path = team_paths.profiles_dir.join("current.mobileprovision");
        let other_profile_path = team_paths.profiles_dir.join("other.mobileprovision");
        let current_key_path = team_paths.certificates_dir.join("current.key.pem");
        let current_cer_path = team_paths.certificates_dir.join("current.cer");
        let current_p12_path = team_paths.certificates_dir.join("current.p12");
        let other_key_path = team_paths.certificates_dir.join("other.key.pem");
        let other_cer_path = team_paths.certificates_dir.join("other.cer");
        let other_p12_path = team_paths.certificates_dir.join("other.p12");
        for path in [
            &current_profile_path,
            &other_profile_path,
            &current_key_path,
            &current_cer_path,
            &current_p12_path,
            &other_key_path,
            &other_cer_path,
            &other_p12_path,
        ] {
            std::fs::write(path, b"fixture").unwrap();
        }

        let state = SigningState {
            certificates: vec![
                ManagedCertificate {
                    id: "CERT-CURRENT".to_owned(),
                    certificate_type: "83Q87W3TGH".to_owned(),
                    serial_number: "CURRENT".to_owned(),
                    origin: CertificateOrigin::Generated,
                    display_name: None,
                    private_key_path: current_key_path.clone(),
                    certificate_der_path: current_cer_path.clone(),
                    p12_path: current_p12_path.clone(),
                    p12_password_account: "current-password".to_owned(),
                },
                ManagedCertificate {
                    id: "CERT-OTHER".to_owned(),
                    certificate_type: "83Q87W3TGH".to_owned(),
                    serial_number: "OTHER".to_owned(),
                    origin: CertificateOrigin::Generated,
                    display_name: None,
                    private_key_path: other_key_path.clone(),
                    certificate_der_path: other_cer_path.clone(),
                    p12_path: other_p12_path.clone(),
                    p12_password_account: "other-password".to_owned(),
                },
            ],
            profiles: vec![
                ManagedProfile {
                    id: "PROFILE-CURRENT".to_owned(),
                    profile_type: "limited".to_owned(),
                    bundle_id: "dev.orbit.fixture".to_owned(),
                    path: current_profile_path.clone(),
                    uuid: None,
                    certificate_ids: vec!["CERT-CURRENT".to_owned()],
                    device_ids: Vec::new(),
                },
                ManagedProfile {
                    id: "PROFILE-OTHER".to_owned(),
                    profile_type: "limited".to_owned(),
                    bundle_id: "dev.orbit.other".to_owned(),
                    path: other_profile_path.clone(),
                    uuid: None,
                    certificate_ids: vec!["CERT-OTHER".to_owned()],
                    device_ids: Vec::new(),
                },
            ],
        };
        save_state(&project, team_id, &state).unwrap();

        let summary = clean_local_signing_state(&project).unwrap();
        assert_eq!(summary.removed_profiles, 1);
        assert_eq!(summary.removed_certificates, 1);
        assert!(!current_profile_path.exists());
        assert!(!current_p12_path.exists());
        assert!(other_profile_path.exists());
        assert!(other_p12_path.exists());

        let cleaned = load_state(&project, team_id).unwrap();
        assert_eq!(cleaned.profiles.len(), 1);
        assert_eq!(cleaned.profiles[0].id, "PROFILE-OTHER");
        assert_eq!(cleaned.certificates.len(), 1);
        assert_eq!(cleaned.certificates[0].id, "CERT-OTHER");
    }

    #[test]
    fn collects_project_identifier_cleanup_inputs_from_entitlements() {
        let (_temp, mut project) = test_project();
        let entitlements_path = project.root.join("App.entitlements");
        let entitlements = Value::Dictionary(plist::Dictionary::from_iter([
            (
                "com.apple.security.application-groups".to_owned(),
                Value::Array(vec![Value::String("group.dev.orbit.fixture".to_owned())]),
            ),
            (
                "com.apple.developer.in-app-payments".to_owned(),
                Value::Array(vec![Value::String("merchant.dev.orbit.fixture".to_owned())]),
            ),
            (
                "com.apple.developer.icloud-container-identifiers".to_owned(),
                Value::Array(vec![Value::String("iCloud.dev.orbit.fixture".to_owned())]),
            ),
        ]));
        entitlements.to_file_xml(&entitlements_path).unwrap();
        project.manifest.targets[0].entitlements = Some("App.entitlements".into());

        let identifiers = project_entitlement_identifiers(&project).unwrap();
        assert_eq!(
            identifiers,
            ProjectEntitlementIdentifiers {
                app_groups: vec!["group.dev.orbit.fixture".to_owned()],
                merchant_ids_by_mac: HashMap::from([(
                    false,
                    vec!["merchant.dev.orbit.fixture".to_owned()],
                )]),
                cloud_containers: vec!["iCloud.dev.orbit.fixture".to_owned()],
            }
        );
    }
}
