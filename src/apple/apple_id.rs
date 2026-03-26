use std::io::{BufReader, BufWriter};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use apple_srp_client::{G_2048, SrpClient};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cookie_store::serde::json::{
    load as load_cookie_store_json, save_incl_expired_and_nonpersistent as save_cookie_store_json,
};
use getrandom::fill as fill_random;
use pbkdf2::pbkdf2_hmac;
use reqwest::StatusCode;
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description;

use crate::util::{CliSpinner, prompt_input, prompt_select};

const APP_STORE_CONNECT_BASE_URL: &str = "https://appstoreconnect.apple.com";
const APPLE_ID_BASE_URL: &str = "https://idmsa.apple.com";
const APPLE_DEVELOPER_PORTAL_TEAMS_URL: &str =
    "https://developer.apple.com/services-account/QH65B2/account/listTeams.action";
const USER_AGENT: &str = concat!("orbit/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum AppleIdError {
    #[error("invalid Apple ID username or password")]
    InvalidCredentials,
    #[error("Apple ID authentication requires an interactive terminal for two-factor verification")]
    InteractiveTwoFactorRequired,
    #[error(
        "Apple ID authentication requires acknowledging updated account terms or security prompts in the browser"
    )]
    ActionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAppleSession {
    pub cookies_json: String,
}

#[derive(Debug, Clone)]
pub struct AppleAuthResponse {
    pub session: StoredAppleSession,
    pub provider_id: Option<String>,
    pub team_id: Option<String>,
    pub provider_name: Option<String>,
}

pub fn restore_session(
    session: &StoredAppleSession,
    team_id: Option<&str>,
    provider_id: Option<&str>,
    interactive: bool,
) -> Result<Option<AppleAuthResponse>> {
    let mut client = AppleIdClient::from_session(session)?;
    let Some(olympus) = client.fetch_olympus_session()? else {
        return Ok(None);
    };
    let provider = client.select_provider(olympus, team_id, provider_id, interactive)?;
    let portal_team_id = client.resolve_portal_team_id(team_id, interactive, provider.as_ref())?;
    Ok(Some(AppleAuthResponse {
        session: client.serialize_session()?,
        provider_id: provider
            .as_ref()
            .and_then(StoredProvider::preferred_provider_id),
        team_id: portal_team_id.or_else(|| {
            provider
                .as_ref()
                .and_then(StoredProvider::preferred_team_id)
        }),
        provider_name: provider.and_then(|provider| provider.name),
    }))
}

pub fn login_with_password(
    apple_id: &str,
    password: &str,
    team_id: Option<&str>,
    provider_id: Option<&str>,
    interactive: bool,
) -> Result<AppleAuthResponse> {
    let mut client = AppleIdClient::new()?;
    let response = client.perform_sirp_login(apple_id, password)?;

    match response.status {
        StatusCode::OK => {}
        StatusCode::CONFLICT => client.handle_two_factor(&response, interactive)?,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
            return Err(AppleIdError::InvalidCredentials.into());
        }
        StatusCode::PRECONDITION_FAILED => {
            return Err(AppleIdError::ActionRequired.into());
        }
        status if response.looks_like_invalid_credentials() => {
            let _ = status;
            return Err(AppleIdError::InvalidCredentials.into());
        }
        _ => bail!(
            "Apple sign-in failed with {}: {}",
            response.status,
            response.error_summary()
        ),
    }

    let olympus = client.fetch_olympus_session()?.context(
        "Apple sign-in completed but App Store Connect session could not be established",
    )?;
    let provider = client.select_provider(olympus, team_id, provider_id, interactive)?;
    let portal_team_id = client.resolve_portal_team_id(team_id, interactive, provider.as_ref())?;

    Ok(AppleAuthResponse {
        session: client.serialize_session()?,
        provider_id: provider
            .as_ref()
            .and_then(StoredProvider::preferred_provider_id),
        team_id: portal_team_id.or_else(|| {
            provider
                .as_ref()
                .and_then(StoredProvider::preferred_team_id)
        }),
        provider_name: provider.and_then(|provider| provider.name),
    })
}

#[derive(Debug, Clone)]
struct AppleIdClient {
    client: Client,
    cookie_store: Arc<CookieStoreMutex>,
    service_key: Option<String>,
    portal_teams: Option<Vec<PortalTeam>>,
    x_apple_id_session_id: Option<String>,
    scnt: Option<String>,
}

impl AppleIdClient {
    fn new() -> Result<Self> {
        let cookie_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
        let client = build_http_client(Arc::clone(&cookie_store))?;
        Ok(Self {
            client,
            cookie_store,
            service_key: None,
            portal_teams: None,
            x_apple_id_session_id: None,
            scnt: None,
        })
    }

    fn from_session(session: &StoredAppleSession) -> Result<Self> {
        let reader = BufReader::new(session.cookies_json.as_bytes());
        let cookie_store = load_cookie_store_json(reader)
            .map_err(|error| anyhow!("failed to parse stored Apple session cookies: {error}"))?;
        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));
        let client = build_http_client(Arc::clone(&cookie_store))?;
        Ok(Self {
            client,
            cookie_store,
            service_key: None,
            portal_teams: None,
            x_apple_id_session_id: None,
            scnt: None,
        })
    }

    fn serialize_session(&self) -> Result<StoredAppleSession> {
        let store = self
            .cookie_store
            .lock()
            .map_err(|_| anyhow!("Apple cookie store is poisoned"))?;
        let mut writer = BufWriter::new(Vec::new());
        save_cookie_store_json(&store, &mut writer)
            .map_err(|error| anyhow!("failed to serialize Apple session cookies: {error}"))?;
        let bytes = writer
            .into_inner()
            .context("failed to flush Apple session cookies")?;
        let cookies_json =
            String::from_utf8(bytes).context("Apple session cookies are not valid UTF-8")?;
        Ok(StoredAppleSession { cookies_json })
    }

    fn fetch_olympus_session(&self) -> Result<Option<OlympusSessionResponse>> {
        let response = self.send_raw(
            self.client
                .get(format!("{APP_STORE_CONNECT_BASE_URL}/olympus/v1/session")),
            "fetch App Store Connect session",
        )?;

        match response.status {
            StatusCode::OK => Ok(Some(response.json()?)),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Ok(None),
            _ => bail!(
                "failed to fetch App Store Connect session ({}): {}",
                response.status,
                response.error_summary()
            ),
        }
    }

    fn perform_sirp_login(&mut self, apple_id: &str, password: &str) -> Result<RawResponse> {
        let service_key = self.fetch_service_key()?.to_owned();
        let mut a_secret = [0u8; 32];
        fill_random(&mut a_secret)
            .map_err(|error| anyhow!("failed to generate Apple SRP secret: {error}"))?;

        let srp = SrpClient::<Sha256>::new(&G_2048);
        let a_pub = srp.compute_public_ephemeral(&a_secret);
        let init_response = self.send_raw(
            self.client
                .post(format!("{APPLE_ID_BASE_URL}/appleauth/auth/signin/init"))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .header(
                    "X-Requested-With",
                    HeaderValue::from_static("XMLHttpRequest"),
                )
                .header("X-Apple-Widget-Key", service_key.as_str())
                .header(
                    ACCEPT,
                    HeaderValue::from_static("application/json, text/javascript"),
                )
                .json(&json!({
                    "a": STANDARD.encode(&a_pub),
                    "accountName": apple_id,
                    "protocols": ["s2k", "s2k_fo"],
                })),
            "initialize Apple SRP login",
        )?;

        if !init_response.status.is_success() {
            if init_response.looks_like_invalid_credentials() {
                return Err(AppleIdError::InvalidCredentials.into());
            }
            bail!(
                "Apple sign-in initialization failed ({}): {}",
                init_response.status,
                init_response.error_summary()
            );
        }

        let init: SrpInitResponse = init_response.json()?;
        let salt = STANDARD
            .decode(init.salt)
            .context("failed to decode Apple SRP salt")?;
        let server_public = STANDARD
            .decode(init.b)
            .context("failed to decode Apple SRP server public value")?;
        let encrypted_password =
            encrypt_password(password.as_bytes(), &salt, init.iteration, &init.protocol)?;
        let verifier = srp
            .process_reply(
                &a_secret,
                apple_id.as_bytes(),
                &encrypted_password,
                &salt,
                &server_public,
            )
            .map_err(|error| anyhow!(error))
            .context("failed to process Apple SRP challenge")?;
        let server_proof = compute_server_proof(&a_pub, verifier.proof(), verifier.key());
        let hashcash = self.fetch_hashcash()?;

        let mut request = self
            .client
            .post(format!(
                "{APPLE_ID_BASE_URL}/appleauth/auth/signin/complete?isRememberMeEnabled=false"
            ))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(
                "X-Requested-With",
                HeaderValue::from_static("XMLHttpRequest"),
            )
            .header("X-Apple-Widget-Key", service_key.as_str())
            .header(
                ACCEPT,
                HeaderValue::from_static("application/json, text/javascript"),
            )
            .json(&json!({
                "accountName": apple_id,
                "c": init.c,
                "m1": STANDARD.encode(verifier.proof()),
                "m2": STANDARD.encode(server_proof),
                "rememberMe": false,
            }));
        if let Some(hashcash) = hashcash {
            request = request.header("X-Apple-HC", hashcash);
        }
        self.send_raw(request, "complete Apple SRP login")
    }

    fn fetch_service_key(&mut self) -> Result<&str> {
        if self.service_key.is_none() {
            let response = self.send_raw(
                self.client.get(format!(
                    "{APP_STORE_CONNECT_BASE_URL}/olympus/v1/app/config?hostname=itunesconnect.apple.com"
                )),
                "fetch App Store Connect auth service key",
            )?;
            if !response.status.is_success() {
                bail!(
                    "failed to fetch the App Store Connect auth service key ({}): {}",
                    response.status,
                    response.error_summary()
                );
            }
            let config: AppConfigResponse = response.json()?;
            if config.auth_service_key.trim().is_empty() {
                bail!("App Store Connect returned an empty auth service key");
            }
            self.service_key = Some(config.auth_service_key);
        }
        Ok(self
            .service_key
            .as_deref()
            .expect("service key was just set"))
    }

    fn fetch_hashcash(&mut self) -> Result<Option<String>> {
        let service_key = self.fetch_service_key()?.to_owned();
        let response = self.send_raw(
            self.client.get(format!(
                "{APPLE_ID_BASE_URL}/appleauth/auth/signin?widgetKey={service_key}"
            )),
            "fetch Apple hashcash challenge",
        )?;
        if !response.status.is_success() {
            return Ok(None);
        }
        let Some(bits) = response.header("X-Apple-HC-Bits") else {
            return Ok(None);
        };
        let Some(challenge) = response.header("X-Apple-HC-Challenge") else {
            return Ok(None);
        };
        Ok(Some(make_hashcash(bits.parse()?, &challenge)?))
    }

    fn handle_two_factor(&mut self, login_response: &RawResponse, interactive: bool) -> Result<()> {
        if !interactive {
            return Err(AppleIdError::InteractiveTwoFactorRequired.into());
        }

        self.x_apple_id_session_id = login_response.header("x-apple-id-session-id");
        self.scnt = login_response.header("scnt");

        if self.x_apple_id_session_id.is_none() || self.scnt.is_none() {
            bail!("Apple requested two-factor verification without the required session headers");
        }

        let options_request = self
            .client
            .get(format!("{APPLE_ID_BASE_URL}/appleauth/auth"));
        let options_response =
            self.send_authenticated_raw(options_request, "fetch Apple two-factor options")?;
        if !options_response.status.is_success() {
            bail!(
                "failed to fetch Apple two-factor options ({}): {}",
                options_response.status,
                options_response.error_summary()
            );
        }

        let options: TwoFactorOptions = options_response.json()?;
        if is_modern_two_factor(&options) {
            self.handle_modern_two_factor(options)?;
        } else if !options.trusted_devices.is_empty() {
            self.handle_two_step(options)?;
        } else {
            bail!(
                "Apple requested two-factor verification but no trusted devices or phone numbers were returned"
            );
        }

        let trust_request = self
            .client
            .get(format!("{APPLE_ID_BASE_URL}/appleauth/auth/2sv/trust"));
        let _ = self.send_authenticated_raw(trust_request, "trust Apple two-factor session");
        Ok(())
    }

    fn handle_two_step(&mut self, options: TwoFactorOptions) -> Result<()> {
        let labels = options
            .trusted_devices
            .iter()
            .map(|device| {
                format!(
                    "{} ({})",
                    device.name,
                    device.model_name.as_deref().unwrap_or("trusted device")
                )
            })
            .collect::<Vec<_>>();
        let index = prompt_select("Select a trusted device for the verification code", &labels)?;
        let device = options
            .trusted_devices
            .get(index)
            .context("selected trusted device is out of range")?;

        let request_code = self.client.put(format!(
            "{APPLE_ID_BASE_URL}/appleauth/auth/verify/device/{}/securitycode",
            device.id
        ));
        let request_response =
            self.send_authenticated_raw(request_code, "request Apple two-step code")?;
        if !request_response.status.is_success() {
            bail!(
                "failed to request the Apple two-step code ({}): {}",
                request_response.status,
                request_response.error_summary()
            );
        }

        loop {
            let code = prompt_input("Apple verification code", None)?;
            let verify_request = self
                .client
                .post(format!(
                    "{APPLE_ID_BASE_URL}/appleauth/auth/verify/phone/securitycode"
                ))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .json(&json!({
                    "phoneNumber": {
                        "id": &device.id,
                    },
                    "securityCode": {
                        "code": code,
                    },
                    "mode": "sms",
                }));
            let response =
                self.send_authenticated_raw(verify_request, "verify Apple two-step code")?;
            if response.status.is_success() {
                return Ok(());
            }
            if response.looks_like_verification_code_error() {
                println!("The Apple verification code was incorrect. Try again.");
                continue;
            }
            bail!(
                "failed to verify the Apple two-step code ({}): {}",
                response.status,
                response.error_summary()
            );
        }
    }

    fn handle_modern_two_factor(&mut self, options: TwoFactorOptions) -> Result<()> {
        let code_length = options
            .security_code
            .and_then(|security_code| security_code.length)
            .unwrap_or(6);
        let mut methods = Vec::new();
        if !options.no_trusted_devices {
            methods.push(ModernTwoFactorMethod::TrustedDevice);
        }
        if options.trusted_phone_numbers.len() == 1 {
            methods.push(ModernTwoFactorMethod::Sms {
                phone: options
                    .trusted_phone_numbers
                    .first()
                    .cloned()
                    .context("missing trusted phone number")?,
                request_code: !options.no_trusted_devices,
            });
        } else {
            methods.extend(options.trusted_phone_numbers.iter().cloned().map(|phone| {
                ModernTwoFactorMethod::Sms {
                    phone,
                    request_code: true,
                }
            }));
        }

        if methods.is_empty() {
            bail!("Apple requested modern two-factor verification but no methods were returned");
        }

        let selected = if methods.len() == 1 {
            methods.remove(0)
        } else {
            let labels = methods
                .iter()
                .map(ModernTwoFactorMethod::display_label)
                .collect::<Vec<_>>();
            let index = prompt_select("Select an Apple verification method", &labels)?;
            methods.remove(index)
        };

        match selected {
            ModernTwoFactorMethod::TrustedDevice => self.handle_device_two_factor(code_length),
            ModernTwoFactorMethod::Sms {
                phone,
                request_code,
            } => self.handle_sms_two_factor(phone, code_length, request_code),
        }
    }

    fn handle_device_two_factor(&mut self, code_length: usize) -> Result<()> {
        loop {
            let code = prompt_input(
                &format!("Enter the {code_length}-digit Apple verification code"),
                None,
            )?;
            let verify_request = self
                .client
                .post(format!(
                    "{APPLE_ID_BASE_URL}/appleauth/auth/verify/trusteddevice/securitycode"
                ))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .json(&json!({
                    "securityCode": {
                        "code": code,
                    }
                }));
            let response =
                self.send_authenticated_raw(verify_request, "verify Apple trusted-device code")?;
            if response.status.is_success() {
                return Ok(());
            }
            if response.looks_like_verification_code_error() {
                println!("The Apple verification code was incorrect. Try again.");
                continue;
            }
            bail!(
                "failed to verify the Apple trusted-device code ({}): {}",
                response.status,
                response.error_summary()
            );
        }
    }

    fn handle_sms_two_factor(
        &mut self,
        phone: TrustedPhoneNumber,
        code_length: usize,
        request_code: bool,
    ) -> Result<()> {
        if request_code {
            let request_code = self
                .client
                .put(format!("{APPLE_ID_BASE_URL}/appleauth/auth/verify/phone"))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .json(&json!({
                    "phoneNumber": {
                        "id": &phone.id,
                    },
                    "mode": phone.push_mode.as_deref().unwrap_or("sms"),
                }));
            let response =
                self.send_authenticated_raw(request_code, "request Apple SMS verification code")?;
            if !response.status.is_success() {
                bail!(
                    "failed to request the Apple SMS verification code ({}): {}",
                    response.status,
                    response.error_summary()
                );
            }
        }

        loop {
            let code = prompt_input(
                &format!(
                    "Enter the {code_length}-digit Apple verification code sent to {}",
                    phone.number_with_dial_code
                ),
                None,
            )?;
            let verify_request = self
                .client
                .post(format!(
                    "{APPLE_ID_BASE_URL}/appleauth/auth/verify/phone/securitycode"
                ))
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .json(&json!({
                    "securityCode": {
                        "code": code,
                    },
                    "phoneNumber": {
                        "id": &phone.id,
                    },
                    "mode": phone.push_mode.as_deref().unwrap_or("sms"),
                }));
            let response = self.send_authenticated_raw(verify_request, "verify Apple SMS code")?;
            if response.status.is_success() {
                return Ok(());
            }
            if response.looks_like_verification_code_error() {
                println!("The Apple verification code was incorrect. Try again.");
                continue;
            }
            bail!(
                "failed to verify the Apple SMS code ({}): {}",
                response.status,
                response.error_summary()
            );
        }
    }

    fn select_provider(
        &mut self,
        olympus: OlympusSessionResponse,
        team_id: Option<&str>,
        provider_id: Option<&str>,
        interactive: bool,
    ) -> Result<Option<StoredProvider>> {
        let mut available = olympus.available_providers;
        if let Some(current) = olympus.provider.clone() {
            if !available
                .iter()
                .any(|candidate| candidate.provider_id == current.provider_id)
            {
                available.push(current);
            }
        }

        let current = olympus.provider;

        let desired = if let Some(preferred) = provider_id {
            available
                .iter()
                .find(|provider| provider.matches(preferred))
                .cloned()
                .with_context(|| {
                    format!(
                        "could not find an Apple team/provider matching `{preferred}`; available options: {}",
                        available
                            .iter()
                            .map(StoredProvider::display_label)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?
        } else if let Some(preferred_team_id) = team_id.filter(|team_id| {
            available
                .iter()
                .any(|provider| provider.team_id.as_deref() == Some(*team_id))
        }) {
            available
                .iter()
                .find(|provider| provider.team_id.as_deref() == Some(preferred_team_id))
                .cloned()
                .context("matched Apple team was unexpectedly unavailable")?
        } else if let Some(preferred_team_id) = team_id {
            if let Some(provider) =
                self.find_provider_matching_portal_team(&available, preferred_team_id)?
            {
                provider
            } else if available.len() == 1 {
                available.remove(0)
            } else if available.is_empty() {
                return Ok(None);
            } else if !interactive {
                let desired = available.remove(0);
                println!(
                    "Using the first Apple team in non-interactive mode: {}.",
                    desired.display_label()
                );
                desired
            } else {
                let labels = available
                    .iter()
                    .map(StoredProvider::display_label)
                    .collect::<Vec<_>>();
                let index = prompt_select("Select an Apple team", &labels)?;
                available
                    .into_iter()
                    .nth(index)
                    .context("selected Apple team is out of range")?
            }
        } else if available.len() == 1 {
            available.remove(0)
        } else if available.is_empty() {
            return Ok(None);
        } else if !interactive {
            let desired = available.remove(0);
            println!(
                "Using the first Apple team in non-interactive mode: {}.",
                desired.display_label()
            );
            desired
        } else {
            let labels = available
                .iter()
                .map(StoredProvider::display_label)
                .collect::<Vec<_>>();
            let index = prompt_select("Select an Apple team", &labels)?;
            available
                .into_iter()
                .nth(index)
                .context("selected Apple team is out of range")?
        };

        if current
            .as_ref()
            .is_none_or(|current| current.provider_id != desired.provider_id)
        {
            let response = self.send_raw(
                self.client
                    .post(format!("{APP_STORE_CONNECT_BASE_URL}/olympus/v1/session"))
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                    .header("X-Requested-With", HeaderValue::from_static("olympus-ui"))
                    .json(&json!({
                        "provider": {
                            "providerId": provider_id_value(&desired.provider_id),
                        }
                    })),
                "switch App Store Connect provider",
            )?;
            if !response.status.is_success() {
                bail!(
                    "failed to switch the App Store Connect provider ({}): {}",
                    response.status,
                    response.error_summary()
                );
            }
            let updated = self.fetch_olympus_session()?.context(
                "App Store Connect provider switched but the updated session was unavailable",
            )?;
            return Ok(Some(
                updated
                    .provider
                    .map(|provider| provider.with_fallbacks_from(&desired))
                    .unwrap_or(desired),
            ));
        }

        Ok(Some(desired))
    }

    fn find_provider_matching_portal_team(
        &mut self,
        available: &[StoredProvider],
        preferred_team_id: &str,
    ) -> Result<Option<StoredProvider>> {
        let teams = match self.fetch_portal_teams() {
            Ok(teams) => teams,
            Err(_) => return Ok(None),
        };
        let Some(team) = teams
            .into_iter()
            .find(|team| team.team_id == preferred_team_id)
        else {
            return Ok(None);
        };

        let mut matches = available
            .iter()
            .filter(|provider| provider.name.as_deref() == Some(team.name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return Ok(matches.pop());
        }
        Ok(None)
    }

    fn send_authenticated_raw(
        &mut self,
        request: RequestBuilder,
        context: &str,
    ) -> Result<RawResponse> {
        let service_key = self.fetch_service_key()?.to_owned();
        let session_id = self
            .x_apple_id_session_id
            .as_deref()
            .context("Apple two-factor session id is missing")?;
        let scnt = self
            .scnt
            .as_deref()
            .context("Apple two-factor scnt is missing")?;

        self.send_raw(
            request
                .header("X-Apple-Id-Session-Id", session_id)
                .header("X-Apple-Widget-Key", service_key.as_str())
                .header(ACCEPT, HeaderValue::from_static("application/json"))
                .header("scnt", scnt),
            context,
        )
    }

    fn fetch_portal_teams(&mut self) -> Result<Vec<PortalTeam>> {
        if let Some(teams) = &self.portal_teams {
            return Ok(teams.clone());
        }

        let response = self.send_raw(
            self.client.post(APPLE_DEVELOPER_PORTAL_TEAMS_URL).body(""),
            "fetch Apple Developer Portal teams",
        )?;
        match response.status {
            StatusCode::OK => {
                let payload: PortalTeamsResponse = response.json()?;
                if payload.result_code != 0 {
                    bail!(
                        "Apple Developer Portal returned resultCode {} while listing teams",
                        payload.result_code
                    );
                }
                self.portal_teams = Some(payload.teams.clone());
                Ok(payload.teams)
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Ok(Vec::new()),
            _ => bail!(
                "failed to fetch Apple Developer Portal teams ({}): {}",
                response.status,
                response.error_summary()
            ),
        }
    }

    fn resolve_portal_team_id(
        &mut self,
        preferred_team_id: Option<&str>,
        interactive: bool,
        selected_provider: Option<&StoredProvider>,
    ) -> Result<Option<String>> {
        let teams = match self.fetch_portal_teams() {
            Ok(teams) => teams,
            Err(error) => {
                println!("Could not resolve Apple Developer team ID: {error}");
                return Ok(None);
            }
        };

        let Some(team) =
            select_portal_team(teams, preferred_team_id, interactive, selected_provider)?
        else {
            return Ok(None);
        };
        Ok(Some(team.team_id))
    }

    fn send_raw(&self, request: RequestBuilder, context: &str) -> Result<RawResponse> {
        const MAX_ATTEMPTS: usize = 3;

        let template = request
            .try_clone()
            .with_context(|| format!("failed to clone Apple request while trying to {context}"))?;

        for attempt in 1..=MAX_ATTEMPTS {
            let spinner = CliSpinner::new(progress_message(context, attempt, MAX_ATTEMPTS));
            let attempt_request = template.try_clone().with_context(|| {
                format!("failed to clone Apple request while trying to {context}")
            })?;

            match attempt_request.send() {
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    let body = response.text().with_context(|| {
                        format!("failed to read Apple response body while trying to {context}")
                    })?;
                    if let Some(message) = success_message(context, status) {
                        spinner.finish_success(message);
                    } else {
                        spinner.finish_clear();
                    }
                    return Ok(RawResponse {
                        status,
                        headers,
                        body,
                    });
                }
                Err(error) if attempt < MAX_ATTEMPTS => {
                    spinner.finish_warning(format!(
                        "{}. Retrying after network error: {}",
                        capitalize_context(context),
                        error
                    ));
                    sleep(retry_delay(attempt));
                }
                Err(error) => {
                    spinner.finish_failure(format!(
                        "{} failed after {} attempts",
                        capitalize_context(context),
                        MAX_ATTEMPTS
                    ));
                    return Err(error).with_context(|| format!("failed to {context}"));
                }
            }
        }

        bail!("failed to {context}")
    }
}

fn build_http_client(cookie_store: Arc<CookieStoreMutex>) -> Result<Client> {
    ClientBuilder::new()
        .user_agent(USER_AGENT)
        .cookie_provider(cookie_store)
        .build()
        .context("failed to create the Apple auth HTTP client")
}

fn progress_message(context: &str, attempt: usize, max_attempts: usize) -> String {
    let message = capitalize_context(context);
    if attempt == 1 {
        format!("Apple auth: {message}...")
    } else {
        format!("Apple auth: {message} (attempt {attempt}/{max_attempts})...")
    }
}

fn retry_delay(attempt: usize) -> Duration {
    match attempt {
        1 => Duration::from_millis(500),
        2 => Duration::from_secs(1),
        _ => Duration::from_secs(2),
    }
}

fn success_message(context: &str, status: StatusCode) -> Option<String> {
    if !status.is_success() {
        return None;
    }

    match context {
        "fetch App Store Connect session" => {
            Some("Apple auth: Validated App Store Connect session.".to_owned())
        }
        "fetch Apple Developer Portal teams" => {
            Some("Apple auth: Loaded Apple Developer teams.".to_owned())
        }
        "switch App Store Connect provider" => {
            Some("Apple auth: Switched App Store Connect provider.".to_owned())
        }
        _ => None,
    }
}

fn capitalize_context(context: &str) -> String {
    let mut chars = context.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

#[derive(Debug, Clone)]
struct RawResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl RawResponse {
    fn json<T>(&self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        serde_json::from_str(&self.body).with_context(|| {
            format!(
                "failed to parse Apple response body as JSON: {}",
                summarize_body(&self.body)
            )
        })
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    }

    fn looks_like_invalid_credentials(&self) -> bool {
        self.status == StatusCode::FORBIDDEN
            || self
                .body
                .to_ascii_lowercase()
                .contains("invalid username and password")
            || self.body.to_ascii_lowercase().contains("invalid=\"true\"")
            || self
                .error_summary()
                .to_ascii_lowercase()
                .contains("invalid")
    }

    fn looks_like_verification_code_error(&self) -> bool {
        self.body.to_ascii_lowercase().contains("verification code")
    }

    fn error_summary(&self) -> String {
        if let Ok(value) = serde_json::from_str::<Value>(&self.body) {
            if let Some(errors) = value.get("serviceErrors").and_then(Value::as_array) {
                let messages = errors
                    .iter()
                    .filter_map(Value::as_object)
                    .map(|error| {
                        format!(
                            "{} {}",
                            error
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        )
                        .trim()
                        .to_owned()
                    })
                    .filter(|message| !message.is_empty())
                    .collect::<Vec<_>>();
                if !messages.is_empty() {
                    return messages.join("; ");
                }
            }

            if let Some(errors) = value.get("validationErrors").and_then(Value::as_array) {
                let messages = errors
                    .iter()
                    .filter_map(Value::as_object)
                    .map(|error| {
                        format!(
                            "{} {}",
                            error
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        )
                        .trim()
                        .to_owned()
                    })
                    .filter(|message| !message.is_empty())
                    .collect::<Vec<_>>();
                if !messages.is_empty() {
                    return messages.join("; ");
                }
            }
        }

        summarize_body(&self.body)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AppConfigResponse {
    #[serde(rename = "authServiceKey")]
    auth_service_key: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SrpInitResponse {
    iteration: u32,
    salt: String,
    b: String,
    c: String,
    protocol: String,
}

#[derive(Debug, Clone, Deserialize)]
struct OlympusSessionResponse {
    #[serde(default)]
    provider: Option<StoredProvider>,
    #[serde(default, rename = "availableProviders")]
    available_providers: Vec<StoredProvider>,
}

#[derive(Debug, Clone, Deserialize)]
struct PortalTeamsResponse {
    #[serde(rename = "resultCode")]
    result_code: i64,
    #[serde(default)]
    teams: Vec<PortalTeam>,
}

#[derive(Debug, Clone, Deserialize)]
struct PortalTeam {
    #[serde(rename = "teamId")]
    team_id: String,
    name: String,
    #[serde(rename = "type")]
    team_type: Option<String>,
}

impl PortalTeam {
    fn display_label(&self) -> String {
        match &self.team_type {
            Some(team_type) if !team_type.is_empty() => {
                format!("{} ({}, {})", self.name, self.team_id, team_type)
            }
            _ => format!("{} ({})", self.name, self.team_id),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct StoredProvider {
    // App Store Connect uses the numeric provider id for team selection and submission.
    #[serde(rename = "providerId", deserialize_with = "deserialize_stringish")]
    provider_id: String,
    // Apple also returns a UUID-flavored public provider identifier alongside it.
    #[serde(
        default,
        rename = "publicProviderId",
        deserialize_with = "deserialize_option_stringish"
    )]
    public_provider_id: Option<String>,
    #[serde(default, rename = "teamId")]
    team_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "contentProviderTypes")]
    content_provider_types: Vec<String>,
}

impl StoredProvider {
    fn matches(&self, value: &str) -> bool {
        self.provider_id == value
            || self
                .public_provider_id
                .as_deref()
                .is_some_and(|candidate| candidate == value)
            || self
                .team_id
                .as_deref()
                .is_some_and(|candidate| candidate == value)
    }

    fn preferred_provider_id(&self) -> Option<String> {
        Some(self.provider_id.clone()).filter(|value| looks_like_provider_id(value))
    }

    fn preferred_team_id(&self) -> Option<String> {
        self.team_id
            .clone()
            .filter(|value| looks_like_apple_team_id(value))
    }

    fn with_fallbacks_from(mut self, fallback: &StoredProvider) -> Self {
        if self.public_provider_id.is_none() {
            self.public_provider_id = fallback.public_provider_id.clone();
        }
        if self.team_id.is_none() {
            self.team_id = fallback.team_id.clone();
        }
        if self.name.is_none() {
            self.name = fallback.name.clone();
        }
        if self.content_provider_types.is_empty() {
            self.content_provider_types = fallback.content_provider_types.clone();
        }
        self
    }

    fn display_label(&self) -> String {
        let name = self
            .name
            .clone()
            .unwrap_or_else(|| self.provider_id.clone());
        let suffix = self
            .team_id
            .clone()
            .or_else(|| self.preferred_provider_id())
            .or_else(|| self.public_provider_id.clone())
            .unwrap_or_else(|| self.provider_id.clone());
        if self.content_provider_types.is_empty() {
            format!("{name} ({suffix})")
        } else {
            format!(
                "{name} [{}] ({suffix})",
                self.content_provider_types.join(", ")
            )
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TwoFactorOptions {
    #[serde(default, rename = "trustedDevices")]
    trusted_devices: Vec<TrustedDevice>,
    #[serde(default, rename = "trustedPhoneNumbers")]
    trusted_phone_numbers: Vec<TrustedPhoneNumber>,
    #[serde(default, rename = "noTrustedDevices")]
    no_trusted_devices: bool,
    #[serde(default, rename = "securityCode")]
    security_code: Option<SecurityCodeInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct SecurityCodeInfo {
    #[serde(default)]
    length: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
struct TrustedDevice {
    #[serde(deserialize_with = "deserialize_stringish")]
    id: String,
    name: String,
    #[serde(default, rename = "modelName")]
    model_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TrustedPhoneNumber {
    #[serde(deserialize_with = "deserialize_stringish")]
    id: String,
    #[serde(rename = "numberWithDialCode")]
    number_with_dial_code: String,
    #[serde(default, rename = "pushMode")]
    push_mode: Option<String>,
}

#[derive(Debug, Clone)]
enum ModernTwoFactorMethod {
    TrustedDevice,
    Sms {
        phone: TrustedPhoneNumber,
        request_code: bool,
    },
}

impl ModernTwoFactorMethod {
    fn display_label(&self) -> String {
        match self {
            Self::TrustedDevice => "Use a code from one of your trusted Apple devices".to_owned(),
            Self::Sms { phone, .. } => {
                format!(
                    "Send a verification code to {}",
                    phone.number_with_dial_code
                )
            }
        }
    }
}

fn is_modern_two_factor(options: &TwoFactorOptions) -> bool {
    options.security_code.is_some()
        || options.no_trusted_devices
        || !options.trusted_phone_numbers.is_empty()
}

fn select_portal_team(
    mut teams: Vec<PortalTeam>,
    preferred_team_id: Option<&str>,
    interactive: bool,
    selected_provider: Option<&StoredProvider>,
) -> Result<Option<PortalTeam>> {
    if teams.is_empty() {
        return Ok(None);
    }

    teams.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.team_id.cmp(&right.team_id))
    });

    if let Some(preferred_team_id) = preferred_team_id {
        let team = teams
            .into_iter()
            .find(|team| team.team_id == preferred_team_id)
            .with_context(|| {
                format!("could not find Apple Developer team `{preferred_team_id}`")
            })?;
        return Ok(Some(team));
    }

    if teams.len() == 1 {
        return Ok(teams.into_iter().next());
    }

    if let Some(provider) = selected_provider {
        let mut name_matches = teams
            .iter()
            .filter(|team| {
                provider
                    .name
                    .as_deref()
                    .is_some_and(|name| team.name == name)
            })
            .cloned()
            .collect::<Vec<_>>();
        if name_matches.len() == 1 {
            return Ok(name_matches.pop());
        }
    }

    if !interactive {
        return Ok(teams.into_iter().next());
    }

    let labels = teams
        .iter()
        .map(PortalTeam::display_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Select an Apple Developer team", &labels)?;
    Ok(teams.into_iter().nth(index))
}

fn encrypt_password(
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    protocol: &str,
) -> Result<Vec<u8>> {
    let mut password_material = Sha256::digest(password).to_vec();
    match protocol {
        "s2k" => {}
        "s2k_fo" => {
            password_material = hex_lower(&password_material).into_bytes();
        }
        other => bail!("unsupported Apple SRP protocol `{other}`"),
    }

    let mut encrypted = vec![0u8; 32];
    pbkdf2_hmac::<Sha256>(&password_material, salt, iterations, &mut encrypted);
    Ok(encrypted)
}

// Apple’s hashcash format drops the random field and uses a decimal counter.
fn make_hashcash(bits: usize, challenge: &str) -> Result<String> {
    let format = format_description::parse("[year][month][day][hour][minute][second]")?;
    let date = OffsetDateTime::now_utc().format(&format)?;

    let mut counter = 0u64;
    loop {
        let candidate = format!("1:{bits}:{date}:{challenge}::{counter}");
        let digest = Sha1::digest(candidate.as_bytes());
        if has_leading_zero_bits(digest.as_slice(), bits) {
            return Ok(candidate);
        }
        counter = counter
            .checked_add(1)
            .context("Apple hashcash counter overflowed")?;
    }
}

fn has_leading_zero_bits(bytes: &[u8], bits: usize) -> bool {
    let full_bytes = bits / 8;
    let remainder = bits % 8;

    if bytes.iter().take(full_bytes).any(|byte| *byte != 0) {
        return false;
    }
    if remainder == 0 {
        return true;
    }

    bytes
        .get(full_bytes)
        .is_some_and(|byte| byte >> (8 - remainder) == 0)
}

fn provider_id_value(provider_id: &str) -> Value {
    provider_id
        .parse::<u64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::from(provider_id.to_owned()))
}

fn looks_like_apple_team_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn looks_like_provider_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn summarize_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "<empty response>".to_owned()
    } else {
        trimmed.chars().take(240).collect()
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(hex_digit(byte >> 4));
        output.push(hex_digit(byte & 0x0f));
    }
    output
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'a' + (nibble - 10)) as char,
        _ => unreachable!("nibble must be in range 0..=15"),
    }
}

fn compute_server_proof(a_pub: &[u8], proof: &[u8], key: &[u8]) -> Vec<u8> {
    let mut digest = Sha256::new();
    digest.update(a_pub);
    digest.update(proof);
    digest.update(key);
    digest.finalize().to_vec()
}

fn deserialize_stringish<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(value) => Ok(value),
        Value::Number(value) => Ok(value.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected a string or number, got {other}"
        ))),
    }
}

fn deserialize_option_stringish<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(Value::Number(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected a string or number, got {other}"
        ))),
    }
}
