use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::load_auth_dot_json;

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
    /// Loads every account independently so one bad file does not hide the others.
    pub fn load_credentials(&self) -> CredentialLoadReport {
        let mut loaded = Vec::new();
        let mut errors = Vec::new();

        for profile in &self.profiles {
            match load_auth_dot_json(
                &profile.home,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            ) {
                Ok(Some(credentials)) => {
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
