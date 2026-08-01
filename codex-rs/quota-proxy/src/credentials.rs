use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::load_auth_dot_json;
use codex_login::token_data::parse_jwt_expiration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::PoolSettings;

/// Credentials and display identity loaded for one account.
pub struct LoadedAccountCredentials {
    pub label: String,
    pub identifier: String,
    pub credentials: AuthDotJson,
}

/// Successful loads and account-specific errors from one settings file.
pub struct CredentialLoadReport {
    pub loaded: Vec<LoadedAccountCredentials>,
    pub errors: Vec<String>,
}

impl PoolSettings {
    /// Loads every account independently and renews credentials before returning them.
    pub async fn load_credentials(&self) -> CredentialLoadReport {
        let mut loaded = Vec::new();
        let mut errors = Vec::new();

        for profile in &self.profiles {
            match load_auth_dot_json(
                &profile.home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            ) {
                Ok(Some(mut credentials)) => {
                    // Refresh access tokens that expire within five minutes.
                    let renewal_needed = credentials
                        .tokens
                        .as_ref()
                        .and_then(|tokens| parse_jwt_expiration(&tokens.access_token).ok().flatten())
                        .is_some_and(|expires_at| {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map_or(0, |duration| duration.as_secs() as i64);
                            expires_at.timestamp() <= now + 5 * 60
                        });

                    if renewal_needed {
                        println!("renewing account '{}'", profile.label);
                        let manager = AuthManager::shared(
                            profile.home.clone(),
                            /*enable_codex_api_key_env*/ false,
                            AuthCredentialsStoreMode::File,
                            /*forced_chatgpt_workspace_id*/ None,
                            /*chatgpt_base_url*/ None,
                            AuthKeyringBackendKind::default(),
                            AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                                OutboundProxyPolicy::ReqwestDefault,
                            )),
                        )
                        .await;
                        if let Err(error) = manager.refresh_token().await {
                            errors.push(format!(
                                "account '{}' is unavailable: renewal failed: {error}; run `codex login` with CODEX_HOME set to '{}'",
                                profile.label,
                                profile.home.display()
                            ));
                            continue;
                        }
                        println!("renewed account '{}'", profile.label);

                        // Reload the file written by AuthManager before this account is used.
                        match load_auth_dot_json(
                            &profile.home,
                            AuthCredentialsStoreMode::File,
                            AuthKeyringBackendKind::default(),
                        ) {
                            Ok(Some(renewed)) => credentials = renewed,
                            Ok(None) | Err(_) => {
                                errors.push(format!(
                                    "account '{}' is unavailable: renewed credentials could not be read; run `codex login` with CODEX_HOME set to '{}'",
                                    profile.label,
                                    profile.home.display()
                                ));
                                continue;
                            }
                        }
                    }

                    let identifier = credentials.tokens.as_ref().and_then(|tokens| {
                        tokens
                            .id_token
                            .email
                            .clone()
                            .or_else(|| tokens.account_id.clone())
                            .or_else(|| tokens.id_token.chatgpt_account_id.clone())
                    });
                    match identifier {
                        Some(identifier) => loaded.push(LoadedAccountCredentials {
                            label: profile.label.clone(),
                            identifier,
                            credentials,
                        }),
                        None => errors.push(format!(
                            "account '{}' credentials contain no email or identifier; run `codex login` with CODEX_HOME set to '{}'",
                            profile.label,
                            profile.home.display()
                        )),
                    }
                }
                Ok(None) => errors.push(format!(
                    "account '{}' has no saved credentials in '{}'; run `codex login` with CODEX_HOME set to '{}'",
                    profile.label,
                    profile.home.join("auth.json").display(),
                    profile.home.display()
                )),
                Err(error) => errors.push(format!(
                    "account '{}' credentials in '{}' are unreadable: {error}; run `codex login` with CODEX_HOME set to '{}'",
                    profile.label,
                    profile.home.join("auth.json").display(),
                    profile.home.display()
                )),
            }
        }

        CredentialLoadReport { loaded, errors }
    }
}
