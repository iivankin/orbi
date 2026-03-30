use std::path::Path;

use anyhow::{Context, Result};
use reqwest::Url;
use reqwest::blocking::Response;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};

use crate::apple::authkit::{
    AuthKitIdentity, build_cookie_client, header_map, send_authkit_bootstrap_request,
};
use crate::apple::developer_services::DeveloperServicesClient;
use crate::apple::grand_slam::establish_xcode_notary_auth;
use crate::apple::provisioning::ProvisioningClient;
use crate::cli::{
    AppleAscSessionDebugArgs, AppleDeveloperServicesDebugArgs, AppleNotaryStatusDebugArgs,
};
use crate::context::AppContext;

const NOTARY_BASE_URL: &str = "https://appstoreconnect.apple.com/notary/v2/submissions";

#[derive(serde::Deserialize)]
struct AuthkitBootstrapResponse {
    jwt: Option<String>,
}

#[derive(serde::Deserialize)]
struct NotaryLogDocument {
    data: NotaryLogData,
}

#[derive(serde::Deserialize)]
struct NotaryLogData {
    attributes: NotaryLogAttributes,
}

#[derive(serde::Deserialize)]
struct NotaryLogAttributes {
    #[serde(rename = "developerLogUrl")]
    developer_log_url: String,
}

pub fn debug_notary_status(
    app: &AppContext,
    manifest_path: Option<&Path>,
    args: &AppleNotaryStatusDebugArgs,
) -> Result<()> {
    let team_id = manifest_path
        .map(|path| app.load_project(Some(path)))
        .transpose()?
        .and_then(|project| project.resolved_manifest.team_id)
        .or_else(|| std::env::var("ORBIT_APPLE_TEAM_ID").ok())
        .context("notary status requires team_id in orbit.json or ORBIT_APPLE_TEAM_ID")?;
    let auth = establish_xcode_notary_auth(app)?;
    let client = build_cookie_client("notary debug")?;

    let bootstrap_response =
        send_authkit_bootstrap_request(&client, &auth, AuthKitIdentity::Xcode, "Xcode")?;
    print_response("Notary authkit bootstrap", bootstrap_response)?;

    let status_url = Url::parse(&format!("{NOTARY_BASE_URL}/{}", args.submission_id.trim()))
        .context("failed to build notary status URL")?;
    let status_response = client
        .get(status_url)
        .headers(header_map(&auth.notary_headers(&team_id))?)
        .send()
        .with_context(|| {
            format!(
                "failed to fetch notary status for `{}`",
                args.submission_id.trim()
            )
        })?;
    print_response("Notary status", status_response)?;

    let logs_url = Url::parse(&format!(
        "{NOTARY_BASE_URL}/{}/logs",
        args.submission_id.trim()
    ))
    .context("failed to build notary logs URL")?;
    let logs_response = client
        .get(logs_url)
        .headers(header_map(&auth.notary_headers(&team_id))?)
        .send()
        .with_context(|| {
            format!(
                "failed to fetch notary logs metadata for `{}`",
                args.submission_id.trim()
            )
        })?;
    let logs_status = logs_response.status();
    let logs_headers = logs_response.headers().clone();
    let logs_body = logs_response
        .bytes()
        .context("failed to read notary logs metadata body")?;
    print_response_parts(
        "Notary logs metadata",
        logs_status,
        &logs_headers,
        &logs_body,
    )?;

    if let Ok(log_document) = serde_json::from_slice::<NotaryLogDocument>(&logs_body) {
        let developer_log_response = client
            .get(&log_document.data.attributes.developer_log_url)
            .send()
            .context("failed to download notary developer log")?;
        print_response("Notary developer log", developer_log_response)?;
    }

    Ok(())
}

pub fn debug_asc_session(app: &AppContext, args: &AppleAscSessionDebugArgs) -> Result<()> {
    let auth = establish_xcode_notary_auth(app)?;
    let client = build_cookie_client("ASC session debug")?;

    let bootstrap_response =
        send_authkit_bootstrap_request(&client, &auth, AuthKitIdentity::Xcode, "ASC session")?;
    let bootstrap_status = bootstrap_response.status();
    let bootstrap_headers = bootstrap_response.headers().clone();
    let bootstrap_body = bootstrap_response
        .bytes()
        .context("failed to read ASC authkit bootstrap response body")?;
    print_response_parts(
        "ASC authkit bootstrap",
        bootstrap_status,
        &bootstrap_headers,
        &bootstrap_body,
    )?;
    let bootstrap_payload =
        serde_json::from_slice::<AuthkitBootstrapResponse>(&bootstrap_body).ok();

    let url = Url::parse(args.url.trim()).context("failed to parse ASC probe URL")?;
    let cookie_response = client
        .get(url.clone())
        .send()
        .with_context(|| format!("failed to call ASC probe URL `{}`", url))?;
    print_response("ASC session probe (cookies only)", cookie_response)?;

    let authkit_response = client
        .get(url.clone())
        .headers(header_map(&auth.authkit_headers())?)
        .send()
        .with_context(|| {
            format!(
                "failed to call ASC probe URL `{}` with authkit headers",
                url
            )
        })?;
    print_response("ASC session probe (authkit headers)", authkit_response)?;

    let developer_services_response = client
        .get(url.clone())
        .headers(header_map(&auth.developer_services_headers())?)
        .send()
        .with_context(|| {
            format!(
                "failed to call ASC probe URL `{}` with developer services headers",
                url
            )
        })?;
    print_response(
        "ASC session probe (developer services headers)",
        developer_services_response,
    )?;

    if let Some(jwt) = bootstrap_payload.and_then(|payload| payload.jwt) {
        let bearer_response = client
            .get(url.clone())
            .header(AUTHORIZATION, format!("Bearer {jwt}"))
            .send()
            .with_context(|| {
                format!(
                    "failed to call ASC probe URL `{}` with authkit bearer token",
                    url
                )
            })?;
        print_response("ASC session probe (authkit jwt bearer)", bearer_response)?;
    }

    let orbit_client = build_cookie_client("ASC Orbit identity debug")?;
    let orbit_bootstrap_response =
        send_authkit_bootstrap_request(&orbit_client, &auth, AuthKitIdentity::Orbit, "Orbit")?;
    print_response(
        "ASC authkit bootstrap (Orbit identity)",
        orbit_bootstrap_response,
    )?;

    Ok(())
}

pub fn debug_developer_services(
    app: &AppContext,
    manifest_path: Option<&Path>,
    args: &AppleDeveloperServicesDebugArgs,
) -> Result<()> {
    let team_id = manifest_path
        .map(|path| app.load_project(Some(path)))
        .transpose()?
        .and_then(|project| project.resolved_manifest.team_id)
        .or_else(|| std::env::var("ORBIT_APPLE_TEAM_ID").ok())
        .context(
            "developer services debug requires team_id in orbit.json or ORBIT_APPLE_TEAM_ID",
        )?;

    let mut developer_services = DeveloperServicesClient::authenticate(app)?;
    let teams = developer_services.list_teams()?;
    println!("Developer Services teams:");
    for team in &teams {
        match team.team_type.as_deref() {
            Some(team_type) if !team_type.is_empty() => {
                println!("  - {} ({}, {})", team.name, team.team_id, team_type);
            }
            _ => println!("  - {} ({})", team.name, team.team_id),
        }
    }

    if let Some(bundle_id) = args.bundle_id.as_deref() {
        let mut provisioning = ProvisioningClient::authenticate(app, team_id.clone())?;
        let bundle = provisioning.find_bundle_id(bundle_id)?;
        match bundle {
            Some(bundle) => {
                println!(
                    "\nBundle ID: {} ({}) seed={} capabilities={}",
                    bundle.identifier,
                    bundle.name,
                    bundle.seed_id,
                    bundle.capabilities.len()
                );
            }
            None => {
                println!("\nBundle ID `{bundle_id}` not found.");
            }
        }
    }

    if let Some(certificate_type) = args.certificate_type.as_deref() {
        let mut provisioning = ProvisioningClient::authenticate(app, team_id)?;
        let certificates = provisioning.list_certificates(certificate_type)?;
        println!(
            "\nCertificates for `{certificate_type}`: {}",
            certificates.len()
        );
        for certificate in certificates {
            println!(
                "  - {} serial={:?} name={:?}",
                certificate.id, certificate.serial_number, certificate.display_name
            );
        }
    }

    Ok(())
}

fn print_response(label: &str, response: Response) -> Result<()> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .bytes()
        .context("failed to read response body for debug request")?;

    print_response_parts(label, status, &headers, &body)
}

fn print_response_parts(
    label: &str,
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<()> {
    println!("{label} response");
    println!("  status: {status}");
    if let Some(content_type) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        println!("  content_type: {content_type}");
    }

    println!("  headers:");
    for (name, value) in headers {
        let rendered = value.to_str().unwrap_or("<non-utf8>");
        println!("    {name}: {rendered}");
    }

    println!("  body:");
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("{}", String::from_utf8_lossy(body));
    }
    Ok(())
}
