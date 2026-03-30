use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use time::format_description;

use super::endpoints;

#[derive(Debug, Clone)]
pub struct AppLookup {
    pub app_id: String,
}

#[derive(Debug, Clone)]
pub struct LabelServiceClient {
    client: Client,
    headers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    result: Option<LookupResult>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct LookupResult {
    #[serde(rename = "Applications", default)]
    applications: BTreeMap<String, String>,
    #[serde(rename = "Attributes", default)]
    attributes: Vec<BTreeMap<String, serde_json::Value>>,
    #[serde(rename = "Success", default)]
    success: bool,
}

impl LabelServiceClient {
    pub fn from_headers(headers: BTreeMap<String, String>) -> Result<Self> {
        let client = ClientBuilder::new()
            .brotli(true)
            .gzip(true)
            .deflate(true)
            .build()
            .context("failed to build the LabelService HTTP client")?;
        Ok(Self { client, headers })
    }

    pub fn lookup_software_for_bundle_id(
        &self,
        bundle_id: &str,
        software_type: &str,
    ) -> Result<AppLookup> {
        let request_id = request_id()?;
        let request_body = json!({
            "id": request_id,
            "jsonrpc": "2.0",
            "method": "lookupSoftwareForBundleId",
            "params": {
                "Application": "TransporterApp",
                "ApplicationBundleId": "com.apple.TransporterApp",
                "BundleId": bundle_id,
                "OSIdentifier": host_os_identifier()?,
                "SoftwareTypeEnum": software_type,
                "Version": "1.4 (14025)"
            }
        });

        let response = self
            .client
            .post(endpoints::label_service_url())
            .headers(request_headers(&self.headers, &request_id)?)
            .json(&request_body)
            .send()
            .context("failed to call lookupSoftwareForBundleId")?;
        if !response.status().is_success() {
            bail!(
                "lookupSoftwareForBundleId failed with {}",
                response.status()
            );
        }
        let response: JsonRpcResponse = response
            .json()
            .context("failed to decode lookupSoftwareForBundleId response")?;
        if let Some(error) = response.error {
            bail!(
                "lookupSoftwareForBundleId failed with {}: {}",
                error.code,
                error.message
            );
        }
        let result = response
            .result
            .context("lookupSoftwareForBundleId did not return a result")?;
        if !result.success {
            bail!("lookupSoftwareForBundleId returned Success=false");
        }

        let app_id = result
            .attributes
            .iter()
            .find_map(|entry| entry.get("Apple ID")?.as_str())
            .map(ToOwned::to_owned)
            .or_else(|| result.applications.values().next().cloned())
            .context("lookupSoftwareForBundleId did not include an Apple ID")?;
        Ok(AppLookup { app_id })
    }
}

fn request_headers(
    snapshot_headers: &BTreeMap<String, String>,
    request_id: &str,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in snapshot_headers {
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("accept-encoding")
        {
            continue;
        }
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(request_id)?,
    );
    headers.insert(
        HeaderName::from_static("x-tx-method"),
        HeaderValue::from_static("lookupSoftwareForBundleId"),
    );
    headers.insert(
        HeaderName::from_static("x-tx-client-name"),
        HeaderValue::from_static("TransporterApp"),
    );
    headers.insert(
        HeaderName::from_static("x-tx-client-version"),
        HeaderValue::from_static("1.4 (14025)"),
    );
    Ok(headers)
}

fn request_id() -> Result<String> {
    let format =
        format_description::parse("[year][month][day][hour][minute][second]-[subsecond digits:3]")
            .context("failed to build lookup request-id formatter")?;
    OffsetDateTime::now_utc()
        .format(&format)
        .context("failed to format lookup request id")
}

fn host_os_identifier() -> Result<String> {
    if let Ok(value) = std::env::var("ORBIT_SUBMIT_HOST_OS_IDENTIFIER") {
        return Ok(value);
    }

    let version =
        crate::util::command_output(std::process::Command::new("sw_vers").arg("-productVersion"))
            .unwrap_or_else(|_| "26.0.0".to_owned());
    let arch = crate::util::command_output(std::process::Command::new("uname").arg("-m"))
        .unwrap_or_else(|_| std::env::consts::ARCH.to_owned());
    Ok(format!("Mac OS X {} ({})", version.trim(), arch.trim()))
}
