//! Hugging Face credentials.
//!
//! Some models are gated: the repository exists, but downloading it requires a
//! Hugging Face account that has accepted the model's licence. Loquara stores
//! that account's access token in the operating system's credential store —
//! never in the app database, which is a plain unencrypted SQLite file sitting
//! in the user's profile.

use keyring::Entry;

const SERVICE: &str = "Loquara";
const ACCOUNT: &str = "huggingface-token";

/// Failures a caller can do something about. The underlying keyring errors are
/// not surfaced verbatim because they name platform APIs the user cannot act on.
#[derive(Debug, PartialEq, Eq)]
pub enum CredentialError {
    /// The OS credential store could not be reached at all.
    Unavailable(String),
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(detail) => {
                write!(formatter, "The credential store is unavailable: {detail}")
            }
        }
    }
}

fn entry() -> Result<Entry, CredentialError> {
    Entry::new(SERVICE, ACCOUNT).map_err(|error| CredentialError::Unavailable(error.to_string()))
}

/// The stored token, or `None` when the user has not connected an account.
///
/// A store that cannot be reached reads as "no token": the download then fails
/// with the same gated-repository message as it would without one, which is
/// the outcome the user needs to see either way.
pub fn stored_token() -> Option<String> {
    let entry = entry().ok()?;
    match entry.get_password() {
        Ok(token) if !token.trim().is_empty() => Some(token),
        _ => None,
    }
}

/// Replaces the stored token.
pub fn store_token(token: &str) -> Result<(), CredentialError> {
    entry()?
        .set_password(token)
        .map_err(|error| CredentialError::Unavailable(error.to_string()))
}

/// Removes the stored token. Succeeds when there was nothing to remove.
pub fn clear_token() -> Result<(), CredentialError> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CredentialError::Unavailable(error.to_string())),
    }
}

/// Trims a pasted token and rejects what cannot be a credential.
///
/// Users paste from a browser, so leading and trailing whitespace is routine.
/// Anything containing inner whitespace is a copy that caught surrounding page
/// text, and would otherwise be stored and fail later as "invalid token".
pub fn normalize_token(raw: &str) -> Result<String, String> {
    let token = raw.trim();
    if token.is_empty() {
        return Err("The access token is empty.".into());
    }
    if token.split_whitespace().count() > 1 {
        return Err("The access token contains spaces.".into());
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_tokens_lose_surrounding_whitespace() {
        assert_eq!(normalize_token("  hf_abc123\n").unwrap(), "hf_abc123");
    }

    #[test]
    fn an_empty_paste_is_rejected_before_it_reaches_the_store() {
        assert_eq!(
            normalize_token("   \n ").unwrap_err(),
            "The access token is empty."
        );
    }

    #[test]
    fn a_paste_that_caught_page_text_is_rejected() {
        // Would otherwise be stored and only fail on the next download.
        assert_eq!(
            normalize_token("Your token: hf_abc123").unwrap_err(),
            "The access token contains spaces."
        );
    }

    #[test]
    fn credential_errors_describe_the_store_not_the_platform_api() {
        let error = CredentialError::Unavailable("no backend".into());

        assert_eq!(
            error.to_string(),
            "The credential store is unavailable: no backend"
        );
    }
}
