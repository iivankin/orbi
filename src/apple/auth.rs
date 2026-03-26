use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cli::{AuthMode, LoginArgs};
use crate::context::AppContext;
use crate::util::{
    command_output, command_output_allow_failure, prompt_input, prompt_password, read_json_file_if_exists,
    write_json_file,
};

const APPLE_PASSWORD_SERVICE: &str = "dev.orbit.cli.apple-password";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AuthState {
    last_mode: Option<StoredAuthMode>,
    user: Option<UserAuth>,
    api_key: Option<ApiKeyAuth>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredAuthMode {
    User,
    ApiKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAuth {
    pub apple_id: String,
    pub team_id: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UserAuthWithPassword {
    pub user: UserAuth,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyAuth {
    pub api_key_path: PathBuf,
    pub key_id: String,
    pub issuer_id: String,
    pub team_id: Option<String>,
    pub team_type: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SubmitAuth {
    ApiKey {
        key_id: String,
        issuer_id: String,
        api_key_path: PathBuf,
    },
    AppleId {
        apple_id: String,
        password: String,
        provider_id: Option<String>,
    },
}

pub fn login(app: &AppContext, args: &LoginArgs) -> Result<()> {
    let mut state = load_state(app)?;
    let mode = match args.mode {
        Some(AuthMode::User) => StoredAuthMode::User,
        Some(AuthMode::ApiKey) => StoredAuthMode::ApiKey,
        None => default_mode(args),
    };

    match mode {
        StoredAuthMode::User => {
            let apple_id = args
                .apple_id
                .clone()
                .or_else(|| state.user.as_ref().map(|user| user.apple_id.clone()))
                .unwrap_or(prompt_input("Apple ID", None)?);
            let password = prompt_password("Apple password or app-specific password")?;
            store_password(&apple_id, &password)?;
            state.user = Some(UserAuth {
                apple_id,
                team_id: args
                    .team_id
                    .clone()
                    .or_else(|| state.user.as_ref().and_then(|user| user.team_id.clone())),
                provider_id: args
                    .provider_id
                    .clone()
                    .or_else(|| state.user.as_ref().and_then(|user| user.provider_id.clone())),
            });
            state.last_mode = Some(StoredAuthMode::User);
        }
        StoredAuthMode::ApiKey => {
            let api_key_path = match args
                .api_key_path
                .clone()
                .or_else(|| state.api_key.as_ref().map(|auth| auth.api_key_path.clone()))
            {
                Some(path) => path,
                None => PathBuf::from(prompt_input("ASC API key path", None)?),
            };
            let key_id = args
                .key_id
                .clone()
                .or_else(|| state.api_key.as_ref().map(|auth| auth.key_id.clone()))
                .unwrap_or(prompt_input("ASC key ID", None)?);
            let issuer_id = args
                .issuer_id
                .clone()
                .or_else(|| state.api_key.as_ref().map(|auth| auth.issuer_id.clone()))
                .unwrap_or(prompt_input("ASC issuer ID", None)?);

            if !api_key_path.exists() {
                bail!("API key file `{}` does not exist", api_key_path.display());
            }

            state.api_key = Some(ApiKeyAuth {
                api_key_path,
                key_id,
                issuer_id,
                team_id: args
                    .team_id
                    .clone()
                    .or_else(|| state.api_key.as_ref().and_then(|auth| auth.team_id.clone())),
                team_type: args
                    .team_type
                    .clone()
                    .or_else(|| state.api_key.as_ref().and_then(|auth| auth.team_type.clone())),
            });
            state.last_mode = Some(StoredAuthMode::ApiKey);
        }
    }

    save_state(app, &state)?;
    status(app)
}

pub fn status(app: &AppContext) -> Result<()> {
    let state = load_state(app)?;
    if let Some(api_key) = resolve_api_key_auth(app)? {
        println!("mode: api-key");
        println!("key_id: {}", api_key.key_id);
        println!("issuer_id: {}", api_key.issuer_id);
        println!("api_key_path: {}", api_key.api_key_path.display());
        if let Some(team_id) = api_key.team_id {
            println!("team_id: {team_id}");
        }
    } else if let Some(user) = resolve_user_auth(app)? {
        println!("mode: user");
        println!("apple_id: {}", user.user.apple_id);
        if let Some(team_id) = user.user.team_id {
            println!("team_id: {team_id}");
        }
        if let Some(provider_id) = user.user.provider_id {
            println!("provider_id: {provider_id}");
        }
        println!("password_source: keychain");
    } else if state.last_mode.is_some() {
        println!("auth metadata is present, but required secrets are missing");
    } else {
        println!("no Apple credentials configured");
    }
    Ok(())
}

pub fn logout(app: &AppContext) -> Result<()> {
    if let Some(user) = load_state(app)?.user {
        let _ = delete_password(&user.apple_id);
    }
    if app.global_paths.auth_state_path.exists() {
        fs::remove_file(&app.global_paths.auth_state_path).with_context(|| {
            format!(
                "failed to remove {}",
                app.global_paths.auth_state_path.display()
            )
        })?;
    }
    Ok(())
}

pub fn resolve_submit_auth(app: &AppContext) -> Result<SubmitAuth> {
    if let Some(api_key) = resolve_api_key_auth(app)? {
        return Ok(SubmitAuth::ApiKey {
            key_id: api_key.key_id,
            issuer_id: api_key.issuer_id,
            api_key_path: api_key.api_key_path,
        });
    }

    let user = resolve_user_auth(app)?
        .context("submit requires App Store Connect API key auth or Apple ID credentials")?;
    Ok(SubmitAuth::AppleId {
        apple_id: user.user.apple_id,
        password: user.password,
        provider_id: user.user.provider_id,
    })
}

pub fn resolve_api_key_auth(app: &AppContext) -> Result<Option<ApiKeyAuth>> {
    let env_path = env_path(["ORBIT_ASC_API_KEY_PATH", "EXPO_ASC_API_KEY_PATH"])?;
    let env_key_id = env_string(["ORBIT_ASC_KEY_ID", "EXPO_ASC_KEY_ID"]);
    let env_issuer_id = env_string(["ORBIT_ASC_ISSUER_ID", "EXPO_ASC_ISSUER_ID"]);
    let env_team_id = env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"]);
    let env_team_type = env_string(["ORBIT_APPLE_TEAM_TYPE", "EXPO_APPLE_TEAM_TYPE"]);

    if let (Some(api_key_path), Some(key_id), Some(issuer_id)) = (env_path, env_key_id, env_issuer_id) {
        return Ok(Some(ApiKeyAuth {
            api_key_path,
            key_id,
            issuer_id,
            team_id: env_team_id,
            team_type: env_team_type,
        }));
    }

    Ok(load_state(app)?.api_key)
}

pub fn resolve_user_auth(app: &AppContext) -> Result<Option<UserAuthWithPassword>> {
    let apple_id = env_string(["ORBIT_APPLE_ID", "EXPO_APPLE_ID"]);
    let env_password = env_string(["ORBIT_APPLE_PASSWORD", "EXPO_APPLE_PASSWORD"]);
    let team_id = env_string(["ORBIT_APPLE_TEAM_ID", "EXPO_APPLE_TEAM_ID"]);
    let provider_id = env_string(["ORBIT_APPLE_PROVIDER_ID", "EXPO_APPLE_PROVIDER_ID"]);

    if let Some(apple_id) = apple_id {
        let password = match env_password {
            Some(password) => password,
            None => load_password(&apple_id)?.with_context(|| {
                format!("missing password for Apple ID `{apple_id}` in env or Keychain")
            })?,
        };
        return Ok(Some(UserAuthWithPassword {
            user: UserAuth {
                apple_id,
                team_id,
                provider_id,
            },
            password,
        }));
    }

    let Some(user) = load_state(app)?.user else {
        return Ok(None);
    };
    let password = load_password(&user.apple_id)?;
    Ok(password.map(|password| UserAuthWithPassword { user, password }))
}

fn load_state(app: &AppContext) -> Result<AuthState> {
    Ok(read_json_file_if_exists(&app.global_paths.auth_state_path)?.unwrap_or_default())
}

fn save_state(app: &AppContext, state: &AuthState) -> Result<()> {
    write_json_file(&app.global_paths.auth_state_path, state)
}

fn default_mode(args: &LoginArgs) -> StoredAuthMode {
    if args.api_key_path.is_some() || args.key_id.is_some() || args.issuer_id.is_some() {
        StoredAuthMode::ApiKey
    } else {
        StoredAuthMode::User
    }
}

fn env_string<const N: usize>(keys: [&str; N]) -> Option<String> {
    keys.into_iter().find_map(|key| std::env::var(key).ok())
}

fn env_path<const N: usize>(keys: [&str; N]) -> Result<Option<PathBuf>> {
    let Some(value) = env_string(keys) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if !path.exists() {
        bail!("configured API key path `{}` does not exist", path.display());
    }
    Ok(Some(path))
}

fn store_password(account: &str, password: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account,
        "-s",
        APPLE_PASSWORD_SERVICE,
        "-w",
        password,
    ]);
    command_output(&mut command).map(|_| ())
}

fn load_password(account: &str) -> Result<Option<String>> {
    let mut command = Command::new("security");
    command.args([
        "find-generic-password",
        "-w",
        "-a",
        account,
        "-s",
        APPLE_PASSWORD_SERVICE,
    ]);
    let (success, stdout, _) = command_output_allow_failure(&mut command)?;
    if success {
        return Ok(Some(stdout.trim().to_owned()));
    }
    Ok(None)
}

fn delete_password(account: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "delete-generic-password",
        "-a",
        account,
        "-s",
        APPLE_PASSWORD_SERVICE,
    ]);
    let _ = command_output_allow_failure(&mut command)?;
    Ok(())
}
