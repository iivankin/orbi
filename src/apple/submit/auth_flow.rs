use std::collections::BTreeMap;

use anyhow::Result;

use crate::apple::grand_slam;
use crate::context::ProjectContext;

use super::endpoints;

const MOCK_PROVIDER_PUBLIC_ID: &str = "provider-test";

#[derive(Debug, Clone)]
pub struct LookupServiceAuth {
    headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ProviderUploadAuth {
    pub provider_public_id: String,
    headers: BTreeMap<String, String>,
    session_id: Option<String>,
    shared_secret: Option<String>,
}

pub fn establish_submit_auth(
    project: &ProjectContext,
) -> Result<(LookupServiceAuth, ProviderUploadAuth)> {
    if endpoints::uses_mock_content_delivery() {
        return Ok(mock_submit_auth());
    }

    let (lookup, upload) = grand_slam::establish_submit_auth(
        &project.app,
        project.resolved_manifest.team_id.as_deref(),
    )?;
    Ok((
        LookupServiceAuth {
            headers: lookup.headers,
        },
        ProviderUploadAuth {
            provider_public_id: upload.provider_public_id,
            headers: upload.headers,
            session_id: Some(upload.session_id),
            shared_secret: Some(upload.shared_secret),
        },
    ))
}

impl LookupServiceAuth {
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }
}

impl ProviderUploadAuth {
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn session_auth(&self) -> Option<(&str, &str)> {
        Some((self.session_id.as_deref()?, self.shared_secret.as_deref()?))
    }
}

fn mock_submit_auth() -> (LookupServiceAuth, ProviderUploadAuth) {
    let headers = BTreeMap::from([
        ("X-Apple-GS-Token".to_owned(), "mock-gs-token".to_owned()),
        (
            "X-Apple-I-Client-Time".to_owned(),
            "2026-01-01T00:00:00Z".to_owned(),
        ),
    ]);
    (
        LookupServiceAuth {
            headers: headers.clone(),
        },
        ProviderUploadAuth {
            provider_public_id: MOCK_PROVIDER_PUBLIC_ID.to_owned(),
            headers,
            session_id: Some("upload-session".to_owned()),
            shared_secret: Some("upload-secret".to_owned()),
        },
    )
}
