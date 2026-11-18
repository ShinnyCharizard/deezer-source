use super::{http_error, transport, ApiError, ApiErrorKind, Deezer, SignedOut};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

// One bounded token pair per client/account, released on sign-out or process exit.
// Refreshes share the mutex so simultaneous catalogue reads open one session.
#[derive(Default)]
pub(super) struct GraphqlAuth {
    token: Option<Token>,
    refresh: Option<String>,
}

struct Token {
    value: String,
    until: Instant,
}

async fn read_json(mut response: reqwest::Response, maximum: usize) -> Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if bytes.len() + chunk.len() > maximum {
            bail!("The catalogue response exceeds its size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Invalid catalogue response")
}

fn lifetime(jwt: &str) -> Duration {
    // TLS authenticates the issuer. Claims are used only to shorten this cache.
    let claims = jwt
        .split('.')
        .nth(1)
        .and_then(|claims| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims)
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let seconds = claims
        .as_ref()
        .and_then(|claims| {
            Some(
                claims["exp"]
                    .as_u64()?
                    .saturating_sub(claims["iat"].as_u64()?)
                    .saturating_sub(15),
            )
        })
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(seconds)
}

impl Deezer {
    async fn authenticate_graphql(&self, method: &str, payload: Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}/login/{method}", self.ep.auth))
            .query(&[("jo", "p"), ("rto", "p"), ("i", "p")])
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(transport)?;
        if matches!(response.status().as_u16(), 400 | 401 | 403) {
            return Err(SignedOut.into());
        }
        if !response.status().is_success() {
            return Err(http_error(response).await);
        }
        read_json(response, 64 * 1024).await
    }

    async fn graphql_token(&self, rejected: Option<&str>) -> Result<String> {
        let mut state = self.graphql_auth.lock().await;
        if let Some(token) = &state.token {
            if token.until > Instant::now() && rejected != Some(token.value.as_str()) {
                return Ok(token.value.clone());
            }
        }
        state.token = None;
        let reply = if let Some(refresh) = &state.refresh {
            match self
                .authenticate_graphql("renew", json!({"refresh_token":refresh}))
                .await
            {
                Ok(reply) if reply["jwt"].is_string() => Some(reply),
                Ok(_) => None,
                Err(error) if error.downcast_ref::<SignedOut>().is_some() => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let reply = match reply {
            Some(reply) => reply,
            None => {
                let arl = self.arl.as_deref().ok_or(SignedOut)?;
                self.authenticate_graphql("arl", json!({"arl":arl.as_str()}))
                    .await?
            }
        };
        let jwt = reply["jwt"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 16384)
            .ok_or(SignedOut)?
            .to_owned();
        state.refresh = reply["refresh_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 16384)
            .map(str::to_owned);
        state.token = Some(Token {
            until: Instant::now() + lifetime(&jwt),
            value: jwt.clone(),
        });
        Ok(jwt)
    }

    pub async fn graphql(&self, operation: &str, query: &str, variables: Value) -> Result<Value> {
        let mut rejected = None;
        for attempt in 0..2 {
            let jwt = self.graphql_token(rejected.as_deref()).await?;
            let response = self
                .http
                .post(format!("{}/api", self.ep.pipe))
                .bearer_auth(&jwt)
                .header(reqwest::header::ACCEPT_LANGUAGE, self.language.as_ref())
                .json(&json!({"operationName":operation,"query":query,"variables":variables}))
                .send()
                .await
                .map_err(transport)?;
            if response.status().as_u16() == 401 && attempt == 0 {
                rejected = Some(jwt);
                continue;
            }
            if !response.status().is_success() {
                return Err(http_error(response).await);
            }
            // At most two category replies are in flight per native page.
            let value = read_json(response, 2 * 1024 * 1024).await?;
            let errors = value["errors"]
                .as_array()
                .filter(|errors| !errors.is_empty());
            if let Some(errors) = errors {
                let expired = errors.iter().any(|error| {
                    matches!(
                        error["type"].as_str(),
                        Some("JwtTokenInvalidError" | "JwtTokenExpiredError")
                    )
                });
                if expired && attempt == 0 {
                    rejected = Some(jwt);
                    continue;
                }
                if expired {
                    return Err(SignedOut.into());
                }
                let quota = errors.iter().any(|error| {
                    let message = error["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    message.contains("quota")
                        || message.contains("rate limit")
                        || message.contains("too many")
                });
                return Err(ApiError {
                    kind: if quota {
                        ApiErrorKind::Quota
                    } else {
                        ApiErrorKind::Other
                    },
                    message: "The catalogue query was refused".into(),
                }
                .into());
            }
            return value
                .get("data")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("The catalogue query returned no data"));
        }
        unreachable!("Both read attempts return a result")
    }
}
