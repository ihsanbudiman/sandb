//! ChatGPT OAuth credentials stay in the backend secret store.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::{Router, routing::get};
use chrono::Utc;
use openidconnect::core::{
    CoreIdTokenClaims, CoreIdTokenVerifier, CoreJwsSigningAlgorithm, CoreProviderMetadata,
    CoreTokenResponse, CoreTokenType,
};
use openidconnect::{
    AccessTokenHash, AsyncHttpClient, Audience, ClientId, CsrfToken, HttpRequest, HttpResponse,
    IssuerUrl, Nonce, NonceVerifier, OAuth2TokenResponse, PkceCodeChallenge, TokenResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::core::error::{AppError, ErrorCode};
use crate::core::secrets::SecretStore;

const ACCOUNT: &str = "ai:chatgpt_oauth";
const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub signed_in: bool,
    pub account_label: Option<String>,
    pub usage_enabled: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutView {
    pub revocation_unconfirmed: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Identity {
    issuer: String,
    subject: String,
    email: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Registration {
    client_id: String,
    identity: Option<Identity>,
}

#[derive(Deserialize, Serialize)]
struct Credentials {
    registration: Registration,
    access_token: String,
    refresh_token: Option<String>,
    id_token: String,
    token_type: String,
    scopes: Vec<String>,
    saved_at: i64,
    expires_at: i64,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    auth_time: Option<i64>,
    #[serde(default)]
    audiences: Vec<Audience>,
    #[serde(default)]
    authorized_party: Option<ClientId>,
    #[serde(default)]
    earliest_refresh_at: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize)]
struct Record {
    ext_agent_host_id: String,
    pending: Option<Registration>,
    active: Option<Credentials>,
}

pub struct TokenLease {
    token: String,
    redactions: Vec<String>,
    cancellation: CancellationToken,
}

impl TokenLease {
    pub fn token(&self) -> Result<&str, AppError> {
        if self.cancellation.is_cancelled() {
            return Err(cancelled_error());
        }
        Ok(&self.token)
    }

    pub(crate) fn redact(&self, mut error: AppError) -> AppError {
        error.redact(&self.token);
        for secret in &self.redactions {
            error.redact(secret);
        }
        error
    }

    // A copied bearer header cannot be recalled, so provider I/O must also observe cancellation.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

#[derive(Default)]
struct Runtime {
    login: CancellationToken,
    generation: CancellationToken,
    blocked: bool,
    logging_out: bool,
}

struct LogoutGuard<'a>(&'a Mutex<Runtime>);

impl Drop for LogoutGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).logging_out = false;
    }
}

#[derive(Default)]
pub struct OAuthSession {
    operation: tokio::sync::Mutex<()>,
    runtime: Mutex<Runtime>,
    #[cfg(test)]
    issuer: Option<String>,
    #[cfg(test)]
    pub(crate) inference_base: Option<String>,
    #[cfg(test)]
    browser: Option<Arc<dyn Fn(&str) -> Result<(), AppError> + Send + Sync>>,
    #[cfg(test)]
    timeout: Option<Duration>,
}

impl OAuthSession {
    pub async fn logout(&self, secrets: &dyn SecretStore) -> Result<LogoutView, AppError> {
        {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.logging_out {
                return Err(AppError::invalid("ChatGPT sign-out is already in progress"));
            }
            runtime.logging_out = true;
            runtime.blocked = true;
            runtime.login.cancel();
            runtime.generation.cancel();
        }
        let _logout = LogoutGuard(&self.runtime);
        let _operation = self.operation.lock().await;
        let Some(mut record) = load(secrets)? else {
            return Ok(LogoutView {
                revocation_unconfirmed: false,
            });
        };
        let active = record.active.take();
        if let Some(active) = &active {
            record.pending = Some(active.registration.clone());
        }
        // Clear durable tokens before awaiting revocation so dropping this future cannot restore them.
        save(secrets, &record)?;
        let Some(active) = active else {
            return Ok(LogoutView {
                revocation_unconfirmed: false,
            });
        };
        let confirmed = if let Some(refresh) = active.refresh_token.as_deref() {
            match OAuthHttp::new(self.issuer()) {
                Ok(http) => tokio::time::timeout(
                    Duration::from_secs(20),
                    http.revoke(&active.registration.client_id, refresh),
                )
                .await
                .unwrap_or(false),
                Err(_) => false,
            }
        } else {
            false
        };
        Ok(LogoutView {
            revocation_unconfirmed: !confirmed,
        })
    }

    pub async fn access_token(
        &self,
        secrets: &dyn SecretStore,
        cancel: &CancellationToken,
    ) -> Result<TokenLease, AppError> {
        let generation = {
            let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.blocked {
                return Err(auth_error("Sign in to ChatGPT again"));
            }
            runtime.generation.clone()
        };
        let _operation = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled_error()),
            _ = generation.cancelled() => return Err(cancelled_error()),
            operation = self.operation.lock() => operation,
        };
        // Finish rotation under the lock so a waiting logout can revoke the latest refresh token.
        let result = self.access_token_locked(secrets, &generation).await;
        if cancel.is_cancelled() || (generation.is_cancelled() && result.is_ok()) {
            return Err(cancelled_error());
        }
        result
    }

    async fn access_token_locked(
        &self,
        secrets: &dyn SecretStore,
        generation: &CancellationToken,
    ) -> Result<TokenLease, AppError> {
        if generation.is_cancelled() {
            return Err(cancelled_error());
        }
        let mut record = load(secrets)?.ok_or_else(|| auth_error("Sign in to ChatGPT first"))?;
        let credentials = record
            .active
            .as_mut()
            .ok_or_else(|| auth_error("Sign in to ChatGPT again"))?;
        if !usage_granted(&credentials.scopes) {
            return Err(auth_error("ChatGPT plan usage is not enabled"));
        }
        if credentials.expires_at <= Utc::now().timestamp() + 60 {
            let refresh = credentials
                .refresh_token
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| auth_error("Sign in to ChatGPT again"))?;
            let http = OAuthHttp::new(self.issuer())?;
            let provider = http.discover().await?;
            let reply = http
                .tokens(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &credentials.registration.client_id),
                    ("refresh_token", refresh),
                    ("resource", RESOURCE),
                ])
                .await;
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    if error
                        .server_code
                        .as_deref()
                        .is_some_and(terminal_refresh_error)
                    {
                        record.pending = Some(credentials.registration.clone());
                        record.active = None;
                        let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                        runtime.generation.cancel();
                        runtime.blocked = true;
                        save(secrets, &record)?;
                    }
                    return Err(error);
                }
            };
            let tokens = reply.tokens;
            validate_bearer(&tokens)?;
            if tokens.id_token().is_some() {
                let claims = validate_id(
                    &tokens,
                    &provider,
                    &credentials.registration,
                    |nonce: Option<&Nonce>| match nonce {
                        None => Ok(()),
                        Some(nonce)
                            if credentials
                                .nonce
                                .as_ref()
                                .is_some_and(|old| Nonce::new(old.clone()) == *nonce) =>
                        {
                            Ok(())
                        }
                        _ => Err("Refresh nonce does not match original sign-in".into()),
                    },
                )?;
                if (!credentials.audiences.is_empty()
                    && claims.audiences() != &credentials.audiences)
                    || claims
                        .auth_time()
                        .is_some_and(|t| Some(t.timestamp()) != credentials.auth_time)
                    || claims.authorized_party() != credentials.authorized_party.as_ref()
                    || claims.issue_time().timestamp() < credentials.saved_at - 60
                    || claims.issue_time().timestamp() > Utc::now().timestamp() + 60
                {
                    return Err(auth_error(
                        "ChatGPT refresh identity context changed; sign in again",
                    ));
                }
                credentials.id_token = tokens.id_token().expect("checked above").to_string();
            }
            if let Some(scopes) = tokens.scopes() {
                if scopes
                    .iter()
                    .any(|scope| !credentials.scopes.iter().any(|old| old == scope.as_str()))
                {
                    return Err(auth_error(
                        "ChatGPT refresh unexpectedly expanded its grant",
                    ));
                }
                credentials.scopes = scopes.iter().map(|s| s.as_str().to_owned()).collect();
            }
            credentials.access_token = tokens.access_token().secret().clone();
            if let Some(refresh) = tokens.refresh_token() {
                if refresh.secret().is_empty() {
                    return Err(auth_error("ChatGPT returned an empty refresh token"));
                }
                credentials.refresh_token = Some(refresh.secret().clone());
            }
            credentials.saved_at = Utc::now().timestamp();
            credentials.expires_at = token_expiry(&tokens, credentials.saved_at)?;
            credentials.earliest_refresh_at = reply
                .earliest_refresh_at
                .or(credentials.earliest_refresh_at.take());
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            save(secrets, &record)?;
            if !usage_granted(&record.active.as_ref().expect("active renewal").scopes) {
                runtime.generation.cancel();
                if !runtime.blocked {
                    runtime.generation = CancellationToken::new();
                }
            }
        }
        let credentials = record
            .active
            .as_ref()
            .expect("active credentials remain after renewal");
        if !usage_granted(&credentials.scopes) {
            return Err(auth_error("ChatGPT plan usage is not enabled"));
        }
        if generation.is_cancelled() {
            return Err(cancelled_error());
        }
        Ok(TokenLease {
            token: credentials.access_token.clone(),
            redactions: credentials
                .refresh_token
                .iter()
                .cloned()
                .chain(std::iter::once(credentials.id_token.clone()))
                .collect(),
            cancellation: generation.child_token(),
        })
    }

    pub fn cancel_sign_in(&self) {
        self.runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .login
            .cancel();
    }

    pub fn status(&self, secrets: &dyn SecretStore) -> Result<SessionView, AppError> {
        let record = load(secrets)?;
        let blocked = self
            .runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .blocked;
        let active = record
            .as_ref()
            .and_then(|r| r.active.as_ref())
            .filter(|_| !blocked);
        Ok(SessionView {
            signed_in: active.is_some(),
            account_label: active
                .and_then(|c| c.registration.identity.as_ref())
                .map(|id| id.email.clone().unwrap_or_else(|| id.subject.clone())),
            usage_enabled: active.is_some_and(|c| usage_granted(&c.scopes)),
        })
    }

    pub async fn enable_plan_usage(
        &self,
        secrets: &dyn SecretStore,
    ) -> Result<SessionView, AppError> {
        self.sign_in_inner(secrets, true).await
    }

    pub async fn sign_in(&self, secrets: &dyn SecretStore) -> Result<SessionView, AppError> {
        self.sign_in_inner(secrets, false).await
    }

    async fn sign_in_inner(
        &self,
        secrets: &dyn SecretStore,
        consent: bool,
    ) -> Result<SessionView, AppError> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| AppError::invalid("A ChatGPT session operation is already in progress"))?;
        let cancel = CancellationToken::new();
        {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.logging_out {
                return Err(cancelled_error());
            }
            runtime.login = cancel.clone();
        }
        let timeout = ATTEMPT_TIMEOUT;
        #[cfg(test)]
        let timeout = self.timeout.unwrap_or(timeout);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled_error()),
            result = tokio::time::timeout(timeout, self.attempt(secrets, &cancel, consent)) => {
                result.map_err(|_| {
                    AppError::new(ErrorCode::Timeout, "ChatGPT sign-in timed out; try again")
                })?
            }
        }
    }

    fn issuer(&self) -> &str {
        #[cfg(test)]
        if let Some(issuer) = &self.issuer {
            return issuer;
        }
        ISSUER
    }

    fn open_browser(&self, url: &str) -> Result<(), AppError> {
        #[cfg(test)]
        if let Some(browser) = &self.browser {
            return browser(url);
        }
        open::that(url).map_err(|_| {
            AppError::new(
                ErrorCode::Io,
                "Could not open the system browser for ChatGPT sign-in",
            )
        })
    }

    async fn attempt(
        &self,
        secrets: &dyn SecretStore,
        cancel: &CancellationToken,
        consent: bool,
    ) -> Result<SessionView, AppError> {
        let mut record = load(secrets)?.unwrap_or_else(|| Record {
            ext_agent_host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
            pending: None,
            active: None,
        });
        save(secrets, &record)?;
        let registration = record
            .pending
            .clone()
            .or_else(|| record.active.as_ref().map(|c| c.registration.clone()));
        let client_id = registration
            .as_ref()
            .map(|r| r.client_id.as_str())
            .unwrap_or(DYNAMIC_CLIENT);
        let csrf = CsrfToken::new_random_len(32);
        let nonce = Nonce::new_random_len(32);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| {
                AppError::new(ErrorCode::Io, "Could not start ChatGPT callback listener")
            })?;
        let redirect = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener
                .local_addr()
                .map_err(|_| AppError::internal("Could not read callback address"))?
                .port()
        );
        let (send, receive) = oneshot::channel();
        let app = Router::new()
            .route("/auth/callback", get(callback))
            .with_state(Arc::new(CallbackState {
                csrf: csrf.clone(),
                send: Mutex::new(Some(send)),
            }));
        let mut url =
            openidconnect::url::Url::parse(&format!("{}/api/accounts/authorize", self.issuer()))
                .map_err(|_| auth_error("Invalid ChatGPT authorization endpoint"))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", client_id)
                .append_pair("ext_agent_host_id", &record.ext_agent_host_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect)
                .append_pair("scope", SCOPES)
                .append_pair("resource", RESOURCE)
                .append_pair("state", csrf.secret())
                .append_pair("nonce", nonce.secret())
                .append_pair("code_challenge_method", "S256")
                .append_pair("code_challenge", challenge.as_str());
            if consent {
                q.append_pair("prompt", "consent");
            }
            if registration.is_none() {
                q.append_pair("agent_name_hint", "sandb");
            } else if let Some(active) = &record.active {
                q.append_pair("id_token_hint", &active.id_token);
                if let Some(email) = active
                    .registration
                    .identity
                    .as_ref()
                    .and_then(|id| id.email.as_deref())
                {
                    q.append_pair("login_hint", email);
                }
            }
        }
        // Poll the server with the attempt so cancellation or timeout always drops the listener.
        let result = async {
            self.open_browser(url.as_str())?;
            receive
                .await
                .map_err(|_| auth_error("ChatGPT callback was interrupted"))
        };
        let query = tokio::select! {
            result = result => result?,
            _ = axum::serve(listener, app) => {
                return Err(auth_error("ChatGPT callback listener stopped"));
            }
        };
        if query.contains_key("error") {
            return Err(auth_error("ChatGPT authorization was declined or failed"));
        }
        let code = query
            .get("code")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| auth_error("ChatGPT callback did not include a code"))?;
        let issued = match (&registration, query.get("client_id")) {
            (Some(r), Some(id)) if id != &r.client_id => {
                return Err(auth_error(
                    "ChatGPT registration does not match this sign-in",
                ));
            }
            (Some(r), _) => r.client_id.clone(),
            (None, Some(id))
                if !id.is_empty()
                    && id != DYNAMIC_CLIENT
                    && id.len() <= 1024
                    && id.bytes().all(|b| b.is_ascii_graphic()) =>
            {
                id.clone()
            }
            _ => {
                return Err(auth_error(
                    "ChatGPT registration did not include an issued client ID",
                ));
            }
        };
        let pending = Registration {
            client_id: issued.clone(),
            identity: registration.and_then(|r| r.identity),
        };
        // Keep the issued registration even when exchange fails with invalid_grant.
        record.pending = Some(pending.clone());
        save(secrets, &record)?;

        let http = OAuthHttp::new(self.issuer())?;
        let provider = http.discover().await?;
        let reply = http
            .tokens(&[
                ("grant_type", "authorization_code"),
                ("client_id", &issued),
                ("code", code),
                ("code_verifier", verifier.secret()),
                ("redirect_uri", &redirect),
                ("resource", RESOURCE),
            ])
            .await?;
        let tokens = reply.tokens;
        validate_bearer(&tokens)?;
        let claims = validate_id(&tokens, &provider, &pending, &nonce)?;
        let identity = Identity {
            issuer: claims.issuer().as_str().to_owned(),
            subject: claims.subject().as_str().to_owned(),
            email: claims.email().map(|e| e.as_str().to_owned()),
        };
        let saved_at = Utc::now().timestamp();
        let expires_at = token_expiry(&tokens, saved_at)?;
        record.active = Some(Credentials {
            registration: Registration {
                identity: Some(identity),
                ..pending
            },
            access_token: tokens.access_token().secret().clone(),
            refresh_token: tokens.refresh_token().map(|t| t.secret().clone()),
            id_token: tokens.id_token().expect("validated ID token").to_string(),
            token_type: "Bearer".into(),
            scopes: tokens
                .scopes()
                .map(|s| s.iter().map(|s| s.as_str().to_owned()).collect())
                .unwrap_or_default(),
            saved_at,
            expires_at,
            nonce: Some(nonce.secret().clone()),
            auth_time: claims.auth_time().map(|t| t.timestamp()),
            audiences: claims.audiences().clone(),
            authorized_party: claims.authorized_party().cloned(),
            earliest_refresh_at: reply.earliest_refresh_at,
        });
        record.pending = None;
        {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if cancel.is_cancelled() {
                return Err(cancelled_error());
            }
            save(secrets, &record)?;
            runtime.generation.cancel();
            runtime.generation = CancellationToken::new();
            runtime.blocked = false;
        }
        self.status(secrets)
    }
}

fn usage_granted(scopes: &[String]) -> bool {
    scopes.iter().any(|s| s == "chatgpt.tokens.use.direct")
        && scopes.iter().any(|s| s == "resource.invoke")
}

fn terminal_refresh_error(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant"
            | "invalid_refresh_token"
            | "token_expired"
            | "refresh_token_expired"
            | "refresh_token_invalidated"
            | "refresh_token_reused"
    )
}

fn validate_bearer(tokens: &CoreTokenResponse) -> Result<(), AppError> {
    if tokens.token_type() != &CoreTokenType::Bearer || tokens.access_token().secret().is_empty() {
        return Err(auth_error("ChatGPT did not return a usable bearer token"));
    }
    Ok(())
}

fn token_expiry(tokens: &CoreTokenResponse, saved_at: i64) -> Result<i64, AppError> {
    let lifetime = tokens
        .expires_in()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .filter(|s| *s > 0)
        .ok_or_else(|| auth_error("ChatGPT did not return a valid token lifetime"))?;
    saved_at
        .checked_add(lifetime)
        .ok_or_else(|| auth_error("Invalid ChatGPT token expiry"))
}

fn validate_id(
    tokens: &CoreTokenResponse,
    provider: &CoreProviderMetadata,
    registration: &Registration,
    nonce: impl NonceVerifier,
) -> Result<CoreIdTokenClaims, AppError> {
    let id_token = tokens
        .id_token()
        .ok_or_else(|| auth_error("ChatGPT did not return an ID token"))?;
    let verifier = CoreIdTokenVerifier::new_public_client(
        ClientId::new(registration.client_id.clone()),
        provider.issuer().clone(),
        provider.jwks().clone(),
    )
    .set_allowed_algs([CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256]);
    let claims = id_token
        .claims(&verifier, nonce)
        .map_err(|_| auth_error("ChatGPT ID token validation failed"))?;
    if claims.subject().as_str().is_empty() {
        return Err(auth_error("ChatGPT identity has no subject"));
    }
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            tokens.access_token(),
            id_token
                .signing_alg()
                .map_err(|_| auth_error("Invalid ChatGPT signing algorithm"))?,
            id_token
                .signing_key(&verifier)
                .map_err(|_| auth_error("Invalid ChatGPT signing key"))?,
        )
        .map_err(|_| auth_error("Could not validate ChatGPT access token hash"))?;
        if actual != *expected {
            return Err(auth_error("ChatGPT access token hash does not match"));
        }
    }
    if registration.identity.as_ref().is_some_and(|old| {
        old.issuer != claims.issuer().as_str() || old.subject != claims.subject().as_str()
    }) {
        return Err(auth_error(
            "ChatGPT identity does not match the registered account",
        ));
    }
    Ok(claims.clone())
}

#[derive(Deserialize)]
struct RevocationMetadata {
    issuer: IssuerUrl,
    revocation_endpoint: String,
}

fn json_response<T: serde::de::DeserializeOwned>(response: &HttpResponse) -> Result<T, AppError> {
    let json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if response.status() != StatusCode::OK || !json {
        return Err(auth_error("Invalid ChatGPT response"));
    }
    serde_json::from_slice(response.body()).map_err(|_| auth_error("Invalid ChatGPT response"))
}

#[derive(Deserialize)]
struct TokenReply {
    #[serde(flatten)]
    tokens: CoreTokenResponse,
    earliest_refresh_at: Option<serde_json::Value>,
}

struct OAuthHttp {
    http: reqwest::Client,
    issuer: IssuerUrl,
}

impl OAuthHttp {
    fn new(issuer: &str) -> Result<Self, AppError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| auth_error("Could not initialize ChatGPT HTTP client"))?;
        Ok(Self {
            http,
            issuer: IssuerUrl::new(issuer.to_owned())
                .map_err(|_| auth_error("Invalid ChatGPT issuer"))?,
        })
    }

    async fn discover(&self) -> Result<CoreProviderMetadata, AppError> {
        CoreProviderMetadata::discover_async(self.issuer.clone(), self)
            .await
            .map_err(|_| auth_error("Could not validate ChatGPT discovery and signing keys"))
    }

    async fn post_form(
        &self,
        url: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse, AppError> {
        let body = openidconnect::url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params.iter().copied())
            .finish();
        let request = openidconnect::http::Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json")
            .body(body.into_bytes())
            .map_err(|_| auth_error("Could not build ChatGPT request"))?;
        self.call(request).await
    }

    async fn revoke(&self, client_id: &str, refresh: &str) -> bool {
        for attempt in 0..3 {
            let result = async {
                let request = openidconnect::http::Request::builder()
                    .uri(format!(
                        "{}/.well-known/openid-configuration",
                        self.issuer.as_str()
                    ))
                    .header("accept", "application/json")
                    .body(Vec::new())
                    .map_err(|_| auth_error("Could not build ChatGPT discovery request"))?;
                let response = self.call(request).await?;
                if response.status().is_server_error() {
                    return Err(AppError::new(
                        ErrorCode::AiUpstream,
                        "ChatGPT discovery is temporarily unavailable",
                    ));
                }
                let metadata: RevocationMetadata = json_response(&response)?;
                if metadata.issuer != self.issuer {
                    return Err(auth_error("Untrusted ChatGPT revocation issuer"));
                }
                let response = self
                    .post_form(
                        &metadata.revocation_endpoint,
                        &[
                            ("token", refresh),
                            ("token_type_hint", "refresh_token"),
                            ("client_id", client_id),
                        ],
                    )
                    .await?;
                if response.status().is_server_error() {
                    return Err(AppError::new(
                        ErrorCode::AiUpstream,
                        "ChatGPT revocation is temporarily unavailable",
                    ));
                }
                Ok(response.status() == StatusCode::OK && response.body().is_empty())
            }
            .await;
            match result {
                Ok(confirmed) => return confirmed,
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::ConnectFailed | ErrorCode::Timeout | ErrorCode::AiUpstream
                    ) && attempt < 2 =>
                {
                    tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
                }
                Err(_) => return false,
            }
        }
        false
    }

    async fn tokens(&self, params: &[(&str, &str)]) -> Result<TokenReply, AppError> {
        let response = self
            .post_form(
                &format!("{}/api/accounts/oauth/token", self.issuer.as_str()),
                params,
            )
            .await?;
        if response.status() != StatusCode::OK {
            let mut error = AppError::new(
                ErrorCode::AiUpstream,
                "ChatGPT token request failed; retry later",
            );
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(response.body()) {
                if let Some(code) = value.get("error").and_then(|v| v.as_str()) {
                    if matches!(
                        response.status(),
                        StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED
                    ) && (terminal_refresh_error(code) || code == "invalid_client")
                    {
                        error = auth_error(if code == "invalid_client" {
                            "ChatGPT client configuration was rejected"
                        } else {
                            "ChatGPT session expired; sign in again"
                        });
                        error.server_code = Some(code.to_owned());
                    }
                }
            }
            return Err(error);
        }
        json_response(&response)
    }
}

impl<'c> AsyncHttpClient<'c> for OAuthHttp {
    type Error = AppError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, AppError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let url = openidconnect::url::Url::parse(&request.uri().to_string())
                .map_err(|_| auth_error("Invalid ChatGPT endpoint"))?;
            if url.origin() != self.issuer.url().origin()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(auth_error(
                    "ChatGPT discovery returned an untrusted endpoint",
                ));
            }
            let request = reqwest::Request::try_from(request)
                .map_err(|_| auth_error("Could not build ChatGPT request"))?;
            let mut response = self.http.execute(request).await.map_err(|_| {
                AppError::new(
                    ErrorCode::ConnectFailed,
                    "Could not reach ChatGPT sign-in service",
                )
            })?;
            let mut result = openidconnect::http::Response::builder().status(response.status());
            *result.headers_mut().expect("valid response status") = response.headers().clone();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                AppError::new(
                    ErrorCode::ConnectFailed,
                    "Could not read ChatGPT sign-in response",
                )
            })? {
                if body.len() + chunk.len() > 1024 * 1024 {
                    return Err(auth_error("ChatGPT sign-in response is too large"));
                }
                body.extend_from_slice(&chunk);
            }
            result
                .body(body)
                .map_err(|_| auth_error("Invalid ChatGPT HTTP response"))
        })
    }
}

struct CallbackState {
    csrf: CsrfToken,
    send: Mutex<Option<oneshot::Sender<HashMap<String, String>>>>,
}

async fn callback(
    State(state): State<Arc<CallbackState>>,
    RawQuery(raw): RawQuery,
) -> (StatusCode, &'static str) {
    let raw = raw.unwrap_or_default();
    if raw.len() > 16 * 1024 {
        return (StatusCode::BAD_REQUEST, "Invalid callback");
    }
    let mut query = HashMap::new();
    for (key, value) in openidconnect::url::form_urlencoded::parse(raw.as_bytes()) {
        if query.insert(key.into_owned(), value.into_owned()).is_some() {
            return (StatusCode::BAD_REQUEST, "Invalid callback");
        }
    }
    if !query
        .get("state")
        .is_some_and(|s| CsrfToken::new(s.clone()) == state.csrf)
    {
        return (StatusCode::BAD_REQUEST, "Invalid callback state");
    }
    let mut slot = state.send.lock().unwrap_or_else(|e| e.into_inner());
    let Some(send) = slot.take() else {
        return (StatusCode::GONE, "Callback already used");
    };
    let _ = send.send(query);
    (StatusCode::OK, "You may return to sandb.")
}

fn cancelled_error() -> AppError {
    AppError::new(ErrorCode::Cancelled, "ChatGPT operation was cancelled")
}

fn auth_error(message: &'static str) -> AppError {
    AppError::new(ErrorCode::AuthFailed, message)
}

fn load(secrets: &dyn SecretStore) -> Result<Option<Record>, AppError> {
    secrets
        .get(ACCOUNT)?
        .map(|s| {
            serde_json::from_str(&s).map_err(|_| {
                AppError::new(ErrorCode::SecretStore, "Saved ChatGPT session is damaged")
            })
        })
        .transpose()
}

fn save(secrets: &dyn SecretStore, record: &Record) -> Result<(), AppError> {
    let value = serde_json::to_string(record)
        .map_err(|_| AppError::internal("Could not encode ChatGPT session"))?;
    secrets.set(ACCOUNT, &value)
}

#[cfg(test)]
#[path = "oauth_fixture.rs"]
pub(crate) mod tests;
