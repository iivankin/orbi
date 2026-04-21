use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::P12_PASSWORD_SERVICE;
use crate::context::ProjectContext;
use crate::util::{read_json_file_if_exists, write_json_file};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct SigningState {
    pub(super) certificates: Vec<ManagedCertificate>,
    pub(super) profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CertificateOrigin {
    #[default]
    Generated,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ManagedCertificate {
    pub(super) id: String,
    pub(super) certificate_type: String,
    pub(super) serial_number: String,
    #[serde(default)]
    pub(super) origin: CertificateOrigin,
    pub(super) display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) system_keychain_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) system_signing_identity: Option<String>,
    pub(super) private_key_path: PathBuf,
    pub(super) certificate_der_path: PathBuf,
    pub(super) p12_path: PathBuf,
    pub(super) p12_password_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ManagedProfile {
    pub(super) id: String,
    pub(super) profile_type: String,
    pub(super) bundle_id: String,
    pub(super) path: PathBuf,
    pub(super) uuid: Option<String>,
    pub(super) certificate_ids: Vec<String>,
    pub(super) device_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SigningIdentity {
    pub(super) hash: String,
    pub(super) keychain_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct TeamSigningPaths {
    pub(super) state_path: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) certificates_dir: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) profiles_dir: PathBuf,
}

fn read_certificate_serial_pem(path: &Path) -> Result<String> {
    read_certificate_serial_with_format(path, None)
}

fn read_certificate_serial_with_format(path: &Path, inform: Option<&str>) -> Result<String> {
    let mut command = Command::new("openssl");
    command.arg("x509");
    if let Some(inform) = inform {
        command.args(["-inform", inform]);
    }
    command.args([
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

pub(super) fn team_signing_paths(project: &ProjectContext, team_id: &str) -> TeamSigningPaths {
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

pub(super) fn load_state(project: &ProjectContext, team_id: &str) -> Result<SigningState> {
    let paths = team_signing_paths(project, team_id);
    Ok(read_json_file_if_exists(&paths.state_path)?.unwrap_or_default())
}

pub(super) fn save_state(
    project: &ProjectContext,
    team_id: &str,
    state: &SigningState,
) -> Result<()> {
    let paths = team_signing_paths(project, team_id);
    write_json_file(&paths.state_path, state)
}

pub(super) fn delete_p12_password(account: &str) -> Result<()> {
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

pub(super) fn parse_codesigning_identity_line(line: &str) -> Option<(String, String)> {
    let quote_start = line.find('"')?;
    let quote_end = line[quote_start + 1..].find('"')?;
    let name = line[quote_start + 1..quote_start + 1 + quote_end].to_owned();
    let hash = line.split_whitespace().nth(1)?.trim_matches('"').to_owned();
    Some((hash, name))
}

fn keychain_identities(keychain_path: &str, policy: &str) -> Result<Vec<(String, String)>> {
    let mut find_identity = Command::new("security");
    find_identity.args(["find-identity", "-v", "-p", policy, keychain_path]);
    let output = crate::util::command_output(&mut find_identity)?;
    Ok(output
        .lines()
        .filter_map(parse_codesigning_identity_line)
        .collect())
}

fn user_keychain_paths() -> Result<Vec<PathBuf>> {
    let mut command = Command::new("security");
    command.args(["list-keychains", "-d", "user"]);
    let output = crate::util::command_output(&mut command)?;
    let mut keychains = output
        .lines()
        .map(|line| PathBuf::from(line.trim().trim_matches('"')))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if keychains.is_empty() {
        keychains.push(PathBuf::from("login.keychain-db"));
    }
    Ok(keychains)
}

fn keychain_certificate_records(keychain_path: &str) -> Result<Vec<(String, String)>> {
    let mut command = Command::new("security");
    command.args(["find-certificate", "-a", "-Z", "-p", keychain_path]);
    let output = crate::util::command_output(&mut command)?;
    let mut records = Vec::new();
    let mut current_sha1 = None::<String>;
    let mut current_pem = Vec::new();
    let mut in_pem = false;
    for line in output.lines() {
        if let Some(hash) = line.strip_prefix("SHA-1 hash: ") {
            current_sha1 = Some(hash.trim().to_owned());
            continue;
        }
        if line == "-----BEGIN CERTIFICATE-----" {
            in_pem = true;
            current_pem.clear();
        }
        if in_pem {
            current_pem.push(line.to_owned());
            if line == "-----END CERTIFICATE-----" {
                if let Some(hash) = current_sha1.take() {
                    records.push((hash, current_pem.join("\n")));
                }
                current_pem.clear();
                in_pem = false;
            }
        }
    }
    Ok(records)
}

pub(super) fn recover_system_keychain_identity(
    serial_number: &str,
) -> Result<Option<SigningIdentity>> {
    for keychain_path in user_keychain_paths()? {
        let keychain_str = keychain_path
            .to_str()
            .context("keychain path contains invalid UTF-8")?;
        let mut identities = HashMap::new();
        for policy in ["codesigning", "basic"] {
            for (hash, name) in keychain_identities(keychain_str, policy)? {
                identities.entry(hash).or_insert(name);
            }
        }
        if identities.is_empty() {
            continue;
        }

        for (hash, pem) in keychain_certificate_records(keychain_str)? {
            if !identities.contains_key(&hash) {
                continue;
            }
            let temp = NamedTempFile::new()?;
            fs::write(temp.path(), pem.as_bytes())
                .with_context(|| format!("failed to write {}", temp.path().display()))?;
            let local_serial = read_certificate_serial_pem(temp.path())?;
            if !local_serial.eq_ignore_ascii_case(serial_number) {
                continue;
            }
            return Ok(Some(SigningIdentity {
                hash,
                keychain_path: keychain_path.clone(),
            }));
        }
    }
    Ok(None)
}

pub(super) fn delete_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

pub(super) fn delete_certificate_files(certificate: &ManagedCertificate) -> Result<()> {
    delete_file_if_exists(&certificate.private_key_path)?;
    delete_file_if_exists(&certificate.certificate_der_path)?;
    delete_file_if_exists(&certificate.p12_path)
}
