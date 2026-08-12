//! The anonymous provider: sends nothing, on purpose.
//!
//! Its whole job is to let you ask "is this route actually protected?". A `401`
//! from it is the expected answer, so [`Anonymous::invalidate`] never asks for a
//! retry — there is nothing to refresh, and replaying would just hide the result.

use reqwest::header::HeaderMap;
use url::Url;

use super::{AuthError, AuthProvider, Retry, check_allowed_host};
use crate::redact::{Redactor, Secret};

/// Sends no credential.
#[derive(Debug, Clone)]
pub struct Anonymous {
    name: String,
    allowed_hosts: Vec<String>,
}

impl Anonymous {
    /// Builds an anonymous provider under `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, allowed_hosts: Vec<String>) -> Self {
        Self {
            name: name.into(),
            allowed_hosts,
        }
    }
}

impl AuthProvider for Anonymous {
    fn name(&self) -> &str {
        &self.name
    }

    async fn apply(
        &self,
        _headers: &mut HeaderMap,
        target: &Url,
        _supplied: Option<&Secret>,
    ) -> Result<Redactor, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        Ok(Redactor::new())
    }

    /// Nothing, and that is the answer rather than a failure. A header template
    /// asking for it gets a message saying so.
    async fn credential(
        &self,
        target: &Url,
        _supplied: Option<&Secret>,
    ) -> Result<Option<Secret>, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        Ok(None)
    }

    async fn invalidate(&self) -> Retry {
        Retry::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn injects_nothing_and_never_retries() {
        let provider = Anonymous::new("anonymous", Vec::new());
        let mut headers = HeaderMap::new();
        let url = Url::parse("https://models.internal/v1/chat/completions").unwrap();

        provider.apply(&mut headers, &url, None).await.unwrap();

        assert!(headers.is_empty());
        assert_eq!(provider.invalidate().await, Retry::No);
    }
}
