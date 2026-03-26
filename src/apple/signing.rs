use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use plist::Value;
use serde::{Deserialize, Serialize};

use crate::apple::asc_api::{
    AscClient, BundleIdAttributes, BundleIdCapabilityAttributes, JsonApiDocument,
    ProfileAttributes, RelationshipData, Resource,
};
use crate::apple::auth::resolve_api_key_auth;
use crate::cli::SigningSyncArgs;
use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, DistributionKind, ProfileManifest, TargetManifest};
use crate::util::{
    ensure_dir, prompt_multi_select, prompt_select, read_json_file_if_exists, write_json_file,
};

const P12_PASSWORD_SERVICE: &str = "dev.orbit.cli.codesign-p12";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SigningState {
    certificates: Vec<ManagedCertificate>,
    profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManagedCertificate {
    id: String,
    certificate_type: String,
    serial_number: String,
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
        Some(select_device_udids(project, profile.distribution)?)
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
    let auth = resolve_api_key_auth(&project.app)?
        .context("signing requires App Store Connect API key auth; set ORBIT_ASC_API_KEY_PATH, ORBIT_ASC_KEY_ID, and ORBIT_ASC_ISSUER_ID")?;
    let client = AscClient::new(auth)?;
    let mut state = load_state(project)?;

    let bundle_id = ensure_bundle_id(&client, project, target, platform)?;
    sync_capabilities(&client, &bundle_id, project, target)?;

    let certificate_type = certificate_type(platform, profile)?;
    let certificate = ensure_certificate(&client, project, &mut state, certificate_type)?;
    let profile_type = profile_type(platform, profile)?;
    let device_ids = if matches!(
        profile.distribution,
        DistributionKind::Development | DistributionKind::AdHoc
    ) {
        let selected_udids = device_udids.unwrap_or_default();
        resolve_device_ids(&client, &selected_udids)?
    } else {
        Vec::new()
    };
    let provisioning_profile = ensure_profile(
        &client,
        project,
        &mut state,
        &bundle_id,
        profile_type,
        &certificate,
        &device_ids,
    )?;

    let signing_identity = import_certificate_into_keychain(project, &certificate)?;
    save_state(project, &state)?;

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
    client: &AscClient,
    project: &ProjectContext,
    target: &TargetManifest,
    platform: ApplePlatform,
) -> Result<JsonApiDocument<BundleIdAttributes>> {
    if let Some(bundle_id) = client.find_bundle_id(&target.bundle_id)? {
        return Ok(bundle_id);
    }

    let created = client.create_bundle_id(
        &format!("@orbit/{}", project.manifest.name),
        &target.bundle_id,
        bundle_id_platform(platform),
    )?;
    Ok(JsonApiDocument {
        data: created,
        included: Vec::new(),
    })
}

fn sync_capabilities(
    client: &AscClient,
    bundle_id: &JsonApiDocument<BundleIdAttributes>,
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<()> {
    let Some(entitlements_path) = &target.entitlements else {
        return Ok(());
    };
    let desired = desired_capabilities(&project.root.join(entitlements_path))?;
    let protected = HashSet::from(["GAME_CENTER", "IN_APP_PURCHASE"]);
    let mut existing = HashMap::new();
    for resource in &bundle_id.included {
        if resource.resource_type != "bundleIdCapabilities" {
            continue;
        }
        let attributes: BundleIdCapabilityAttributes =
            serde_json::from_value(resource.attributes.clone())
                .context("failed to parse bundle ID capability")?;
        existing.insert(attributes.capability_type.clone(), resource.id.clone());
    }

    for capability in desired
        .iter()
        .filter(|capability| !existing.contains_key(*capability))
    {
        let _ = client.create_bundle_capability(&bundle_id.data.id, capability)?;
    }

    for (capability, id) in existing {
        if desired.contains(&capability) || protected.contains(capability.as_str()) {
            continue;
        }
        let _ = client.delete_bundle_capability(&id);
    }

    Ok(())
}

fn ensure_certificate(
    client: &AscClient,
    project: &ProjectContext,
    state: &mut SigningState,
    certificate_type: &str,
) -> Result<ManagedCertificate> {
    let remote_certificates = client.list_certificates(certificate_type)?;
    for remote in &remote_certificates {
        if let Some(local) = state
            .certificates
            .iter()
            .find(|certificate| certificate.id == remote.id && certificate.p12_path.exists())
        {
            return Ok(local.clone());
        }
    }

    let certificates_dir = project.app.global_paths.data_dir.join("certificates");
    ensure_dir(&certificates_dir)?;
    let slug = crate::util::timestamp_slug();
    let private_key_path = certificates_dir.join(format!("{slug}.key.pem"));
    let csr_path = certificates_dir.join(format!("{slug}.csr.pem"));
    let certificate_der_path = certificates_dir.join(format!("{slug}.cer"));
    let p12_path = certificates_dir.join(format!("{slug}.p12"));

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
    let remote = client.create_certificate(certificate_type, &csr_pem)?;
    let certificate_content = remote
        .attributes
        .certificate_content
        .clone()
        .context("created certificate did not include certificateContent")?;
    let certificate_bytes = STANDARD
        .decode(certificate_content)
        .context("failed to decode certificateContent")?;
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

    let serial_number = remote
        .attributes
        .serial_number
        .clone()
        .context("created certificate did not include a serial number")?;
    let password_account = format!("{}-{serial_number}", remote.id);
    store_p12_password(&password_account, &p12_password)?;

    let certificate = ManagedCertificate {
        id: remote.id,
        certificate_type: certificate_type.to_owned(),
        serial_number,
        display_name: remote.attributes.display_name.clone(),
        private_key_path,
        certificate_der_path,
        p12_path,
        p12_password_account: password_account,
    };
    state.certificates.push(certificate.clone());
    Ok(certificate)
}

fn ensure_profile(
    client: &AscClient,
    project: &ProjectContext,
    state: &mut SigningState,
    bundle_id: &JsonApiDocument<BundleIdAttributes>,
    profile_type: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
) -> Result<ManagedProfile> {
    let profiles = client.list_profiles(profile_type)?;
    let bundle_identifier = &bundle_id.data.attributes.identifier;
    let mut remote_profile_ids = HashSet::new();
    let mut stale_orbit_profiles = Vec::new();

    for profile in profiles.data {
        remote_profile_ids.insert(profile.id.clone());
        let Some(bundle_link) = profile
            .relationships
            .get("bundleId")
            .and_then(|relationship| match &relationship.data {
                Some(RelationshipData::One(link)) => Some(link.id.as_str()),
                _ => None,
            })
        else {
            continue;
        };
        if bundle_link != bundle_id.data.id {
            continue;
        }

        let certificate_links = profile
            .relationships
            .get("certificates")
            .and_then(|relationship| match &relationship.data {
                Some(RelationshipData::Many(links)) => {
                    Some(links.iter().map(|link| link.id.clone()).collect::<Vec<_>>())
                }
                _ => None,
            })
            .unwrap_or_default();
        let device_links = profile
            .relationships
            .get("devices")
            .and_then(|relationship| match &relationship.data {
                Some(RelationshipData::Many(links)) => {
                    Some(links.iter().map(|link| link.id.clone()).collect::<Vec<_>>())
                }
                _ => None,
            })
            .unwrap_or_default();

        let matches_certificate = certificate_links.contains(&certificate.id);
        let matches_devices = canonical_ids(&device_links) == canonical_ids(device_ids);

        if matches_certificate && matches_devices {
            let managed = persist_profile(
                project,
                state,
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
            .delete_profile(&profile_id)
            .with_context(|| format!("failed to repair provisioning profile `{profile_id}`"))?;
        state.profiles.retain(|profile| profile.id != profile_id);
    }
    cleanup_stale_profile_state(state, bundle_identifier, profile_type, &remote_profile_ids);

    let remote = client.create_profile(
        &format!(
            "*[orbit] {} {} {}",
            bundle_identifier,
            profile_type,
            crate::util::timestamp_slug()
        ),
        profile_type,
        &bundle_id.data.id,
        &[certificate.id.clone()],
        device_ids,
    )?;
    persist_profile(
        project,
        state,
        profile_type,
        bundle_identifier,
        certificate,
        device_ids,
        remote,
    )
}

fn persist_profile(
    project: &ProjectContext,
    state: &mut SigningState,
    profile_type: &str,
    bundle_identifier: &str,
    certificate: &ManagedCertificate,
    device_ids: &[String],
    remote: Resource<ProfileAttributes>,
) -> Result<ManagedProfile> {
    let profiles_dir = project.app.global_paths.data_dir.join("profiles");
    ensure_dir(&profiles_dir)?;
    let profile_content = remote
        .attributes
        .profile_content
        .clone()
        .context("profile response did not include profileContent")?;
    let profile_bytes = STANDARD
        .decode(profile_content)
        .context("failed to decode profileContent")?;
    let profile_path = profiles_dir.join(format!("{}-{}.mobileprovision", remote.id, profile_type));
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;

    state.profiles.retain(|profile| profile.id != remote.id);
    let profile = ManagedProfile {
        id: remote.id,
        profile_type: profile_type.to_owned(),
        bundle_id: bundle_identifier.to_owned(),
        path: profile_path,
        uuid: remote.attributes.uuid.clone(),
        certificate_ids: vec![certificate.id.clone()],
        device_ids: device_ids.to_vec(),
    };
    state.profiles.push(profile.clone());
    Ok(profile)
}

fn resolve_device_ids(client: &AscClient, udids: &[String]) -> Result<Vec<String>> {
    if udids.is_empty() {
        let devices = client.list_devices()?;
        return Ok(devices.into_iter().map(|device| device.id).collect());
    }

    let mut device_ids = Vec::new();
    for udid in udids {
        let device = client
            .find_device_by_udid(udid)?
            .with_context(|| format!("device `{udid}` is not registered with Apple"))?;
        device_ids.push(device.id);
    }
    Ok(device_ids)
}

fn select_device_udids(
    project: &ProjectContext,
    distribution: DistributionKind,
) -> Result<Vec<String>> {
    let cache = crate::apple::device::refresh_cache(&project.app)?;
    if cache.devices.is_empty() {
        bail!("no registered Apple devices found; run `orbit apple device register` first");
    }

    if !project.app.interactive {
        return Ok(cache
            .devices
            .into_iter()
            .map(|device| device.udid)
            .collect());
    }

    let labels = cache
        .devices
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
            .map(|index| cache.devices[index].udid.clone())
            .collect());
    }

    let index = prompt_select("Select a device to provision", &labels)?;
    Ok(vec![cache.devices[index].udid.clone()])
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

fn desired_capabilities(path: &Path) -> Result<HashSet<String>> {
    let value = Value::from_file(path)
        .with_context(|| format!("failed to parse entitlements {}", path.display()))?;
    let dictionary = value
        .into_dictionary()
        .context("entitlements file must contain a top-level dictionary")?;

    let mut capabilities = HashSet::new();
    for (key, value) in &dictionary {
        match key.as_str() {
            "aps-environment" => {
                validate_push_environment(value)?;
                capabilities.insert("PUSH_NOTIFICATIONS".to_owned());
            }
            "com.apple.developer.applesignin" => {
                validate_string_array(key, value)?;
                capabilities.insert("APPLE_ID_AUTH".to_owned());
            }
            "com.apple.security.application-groups" => {
                validate_prefixed_array(key, value, "group.")?;
                capabilities.insert("APP_GROUPS".to_owned());
            }
            "com.apple.developer.in-app-payments" => {
                validate_prefixed_array(key, value, "merchant.")?;
                capabilities.insert("APPLE_PAY".to_owned());
            }
            "com.apple.developer.icloud-container-identifiers" => {
                validate_prefixed_array(key, value, "iCloud.")?;
                capabilities.insert("ICLOUD".to_owned());
            }
            "com.apple.developer.networking.networkextension" => {
                validate_string_array(key, value)?;
                capabilities.insert("NETWORK_EXTENSIONS".to_owned());
            }
            "com.apple.developer.associated-domains" => {
                validate_string_array(key, value)?;
                capabilities.insert("ASSOCIATED_DOMAINS".to_owned());
            }
            _ => {}
        }
    }

    Ok(capabilities)
}

fn validate_push_environment(value: &Value) -> Result<()> {
    let Some(environment) = value.as_string() else {
        bail!("`aps-environment` must be a string");
    };
    match environment {
        "development" | "production" => Ok(()),
        other => bail!("`aps-environment` must be `development` or `production`, got `{other}`"),
    }
}

fn validate_string_array(key: &str, value: &Value) -> Result<()> {
    let Some(values) = value.as_array() else {
        bail!("`{key}` must be an array");
    };
    if values.iter().all(|item| item.as_string().is_some()) {
        Ok(())
    } else {
        bail!("`{key}` must contain only strings")
    }
}

fn validate_prefixed_array(key: &str, value: &Value, prefix: &str) -> Result<()> {
    validate_string_array(key, value)?;
    let values = value.as_array().expect("validated array");
    if values.iter().all(|item| {
        item.as_string()
            .is_some_and(|value| value.starts_with(prefix))
    }) {
        Ok(())
    } else {
        bail!("`{key}` must contain only values prefixed with `{prefix}`")
    }
}

fn certificate_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (ApplePlatform::Ios, DistributionKind::Development) => Ok("IOS_DEVELOPMENT"),
        (ApplePlatform::Ios, DistributionKind::AdHoc | DistributionKind::AppStore) => {
            Ok("IOS_DISTRIBUTION")
        }
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_DISTRIBUTION"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("DEVELOPER_ID_APPLICATION"),
        _ => bail!(
            "signing is not implemented for {platform} with {:?}",
            profile.distribution
        ),
    }
}

fn profile_type(platform: ApplePlatform, profile: &ProfileManifest) -> Result<&'static str> {
    match (platform, profile.distribution) {
        (ApplePlatform::Ios, DistributionKind::Development) => Ok("IOS_APP_DEVELOPMENT"),
        (ApplePlatform::Ios, DistributionKind::AdHoc) => Ok("IOS_APP_ADHOC"),
        (ApplePlatform::Ios, DistributionKind::AppStore) => Ok("IOS_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::Development) => Ok("MAC_APP_DEVELOPMENT"),
        (ApplePlatform::Macos, DistributionKind::MacAppStore) => Ok("MAC_APP_STORE"),
        (ApplePlatform::Macos, DistributionKind::DeveloperId) => Ok("MAC_APP_DIRECT"),
        _ => bail!("provisioning profiles are not implemented for {platform}"),
    }
}

fn bundle_id_platform(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => "MAC_OS",
        _ => "IOS",
    }
}

fn load_state(project: &ProjectContext) -> Result<SigningState> {
    Ok(read_json_file_if_exists(&project.app.global_paths.signing_state_path)?.unwrap_or_default())
}

fn save_state(project: &ProjectContext, state: &SigningState) -> Result<()> {
    write_json_file(&project.app.global_paths.signing_state_path, state)
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
