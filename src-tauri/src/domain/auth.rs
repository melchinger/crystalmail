// Auth abstraction. MVP ships `Password` only; `OAuth2` variant is reserved
// so Gmail/O365 can be added later without touching the IMAP/SMTP sync paths.
//
// The trait owns the "refresh if needed" policy — password credentials are
// always valid, OAuth2 tokens may expire and must be refreshed before use.
//
// Today the IMAP/SMTP sync paths still go directly to `keyring::Entry` and
// only know about `AuthCredential::Password` (via the persisted account
// shape). The `AuthError` / `ResolvedAuth` / `AuthProvider` /
// `KeyringAuthProvider` types below are scaffolding for the OAuth2 work
// — kept alive (and dead-code-allowed) so the migration can grow into
// them without resurrecting them from git history.
#![allow(dead_code)]
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AuthCredential {
    /// Classic IMAP LOGIN / SMTP AUTH PLAIN with an app password.
    /// Secret itself is stored in the OS keyring — only its handle lives here.
    Password { keyring_entry: String },

    /// RFC 7628 SASL XOAUTH2 — not implemented yet. Tokens live in the keyring;
    /// the access token is short-lived and must be refreshed via `refresh_token`.
    #[serde(rename_all = "camelCase")]
    OAuth2 {
        keyring_entry: String,
        #[serde(skip)]
        expires_at: Option<Instant>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("keyring entry not found: {0}")]
    MissingCredential(String),
    #[error("OAuth2 flow not implemented yet")]
    OAuth2NotImplemented,
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),
}

/// Resolved credential ready to be handed to an IMAP/SMTP client.
pub enum ResolvedAuth {
    Password { username: String, password: String },
    #[allow(dead_code)]
    OAuth2 { username: String, access_token: String },
}

pub trait AuthProvider: Send + Sync {
    fn resolve(&self, username: &str, credential: &AuthCredential) -> Result<ResolvedAuth, AuthError>;
}

pub struct KeyringAuthProvider;

impl AuthProvider for KeyringAuthProvider {
    fn resolve(&self, username: &str, credential: &AuthCredential) -> Result<ResolvedAuth, AuthError> {
        match credential {
            AuthCredential::Password { keyring_entry } => {
                let entry = keyring::Entry::new("crystalmail", keyring_entry)?;
                let password = entry
                    .get_password()
                    .map_err(|_| AuthError::MissingCredential(keyring_entry.clone()))?;
                Ok(ResolvedAuth::Password {
                    username: username.to_string(),
                    password,
                })
            }
            AuthCredential::OAuth2 { .. } => Err(AuthError::OAuth2NotImplemented),
        }
    }
}
