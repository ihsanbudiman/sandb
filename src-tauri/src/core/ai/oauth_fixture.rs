use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    AccessToken, Audience, EmptyAdditionalClaims, EndUserEmail, IssuerUrl, JsonWebKeyId, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use serde_json::{Value, json};

use crate::core::secrets::{MemoryStore, VAULT_FILE, VaultStore};

const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const ISSUED_CLIENT: &str = "oaiapp_dummy_fixture";
const ACCESS: &str = "dummy-access-token-not-a-real-credential";
const REFRESH: &str = "dummy-refresh-token-not-a-real-credential";

fn signing_key() -> CoreRsaPrivateSigningKey {
    // This key is public test data, never an application credential.
    CoreRsaPrivateSigningKey::from_pem(
        include_str!("fixtures/oauth-test-rsa.pem"),
        Some(JsonWebKeyId::new("fixture".into())),
    )
    .unwrap()
}

#[derive(Clone, Copy, Debug, Default)]
enum Fault {
    #[default]
    None,
    Nonce,
    Issuer,
    Audience,
    Expired,
    EmptySubject,
    AccessHash,
    Signature,
    Algorithm,
    MissingDirectScope,
    MissingScopes,
    MissingClient,
    DynamicClient,
    ClientMismatch,
    IdentityMismatch,
    InvalidGrant,
    Denied,
    JwksRedirect,
    UntrustedJwks,
    RefreshError(&'static str),
    TransientRefresh,
    RefreshId,
    RefreshSameNonce,
    RefreshAuthTime,
    ExpandedScope,
    RevokeUnavailable,
    RevokeTransient,
    RevokeRedirect,
    RevokeNetwork,
    RevokeUntrusted,
    RevokeIssuer,
    RevokeStalled,
}

struct Provider {
    issuer: String,
    authorization: Mutex<Vec<HashMap<String, String>>>,
    fault: Mutex<Fault>,
    exchanges: AtomicUsize,
    refreshes: AtomicUsize,
    access_lifetime: AtomicU64,
    refresh_lifetime: AtomicU64,
    refresh_expected: Mutex<String>,
    issued_id_token: Mutex<String>,
    revocations: Mutex<Vec<HashMap<String, String>>>,
    block_refresh: AtomicBool,
    refresh_started: tokio::sync::Notify,
    refresh_release: tokio::sync::Notify,
    revoke_started: tokio::sync::Notify,
    revoke_release: tokio::sync::Notify,
}

pub(crate) struct Fixture {
    provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
    browsers: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        for browser in self.browsers.lock().unwrap().drain(..) {
            browser.abort();
        }
    }
}

impl Fixture {
    pub(crate) async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Arc::new(Provider {
            issuer: format!("http://{}", listener.local_addr().unwrap()),
            authorization: Mutex::new(Vec::new()),
            fault: Mutex::new(Fault::None),
            exchanges: AtomicUsize::new(0),
            refreshes: AtomicUsize::new(0),
            access_lifetime: AtomicU64::new(3600),
            refresh_lifetime: AtomicU64::new(3600),
            refresh_expected: Mutex::new(REFRESH.into()),
            issued_id_token: Mutex::new(String::new()),
            revocations: Mutex::new(Vec::new()),
            block_refresh: AtomicBool::new(false),
            refresh_started: tokio::sync::Notify::new(),
            refresh_release: tokio::sync::Notify::new(),
            revoke_started: tokio::sync::Notify::new(),
            revoke_release: tokio::sync::Notify::new(),
        });
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route(
                "/redirect-must-not-be-followed",
                get(|| async {
                    Json(CoreJsonWebKeySet::new(vec![
                        signing_key().as_verification_key(),
                    ]))
                }),
            )
            .route("/api/accounts/authorize", get(authorize))
            .route("/api/accounts/oauth/token", post(token))
            .route("/revoke", post(revoke))
            .route("/revocation-followed", post(|| async { StatusCode::OK }))
            .with_state(provider.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            provider,
            server,
            browsers: Arc::default(),
        }
    }

    pub(crate) fn issued_id_token(&self) -> String {
        self.provider.issued_id_token.lock().unwrap().clone()
    }

    pub(crate) fn session(&self) -> OAuthSession {
        let browsers = self.browsers.clone();
        OAuthSession {
            issuer: Some(self.provider.issuer.clone()),
            browser: Some(Arc::new(move |url| {
                let url = url.to_owned();
                browsers.lock().unwrap().push(tokio::spawn(async move {
                    reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .build()
                        .unwrap()
                        .get(url)
                        .send()
                        .await
                        .unwrap()
                        .error_for_status()
                        .unwrap();
                }));
                Ok(())
            })),
            ..Default::default()
        }
    }

    fn fault(&self, fault: Fault) {
        *self.provider.fault.lock().unwrap() = fault;
    }

    pub(crate) async fn finish_browser(&self) {
        let browsers = std::mem::take(&mut *self.browsers.lock().unwrap());
        for browser in browsers {
            browser.await.unwrap();
        }
    }
}

async fn jwks(State(p): State<Arc<Provider>>) -> Response {
    if matches!(*p.fault.lock().unwrap(), Fault::JwksRedirect) {
        return Redirect::temporary("/redirect-must-not-be-followed").into_response();
    }
    Json(CoreJsonWebKeySet::new(vec![
        signing_key().as_verification_key(),
    ]))
    .into_response()
}

async fn discovery(State(p): State<Arc<Provider>>) -> Json<Value> {
    let origin = if matches!(*p.fault.lock().unwrap(), Fault::UntrustedJwks) {
        p.issuer.replace("127.0.0.1", "localhost")
    } else {
        p.issuer.clone()
    };
    let fault = *p.fault.lock().unwrap();
    let revocation_origin = if matches!(fault, Fault::RevokeUntrusted) {
        p.issuer.replace("127.0.0.1", "localhost")
    } else {
        p.issuer.clone()
    };
    Json(json!({
        "issuer": if matches!(fault, Fault::RevokeIssuer) { "https://wrong.example.test" } else { &p.issuer },
        "authorization_endpoint": format!("{}/api/accounts/authorize", p.issuer),
        "token_endpoint": format!("{}/api/accounts/oauth/token", p.issuer),
        "jwks_uri": format!("{origin}/jwks"),
        "revocation_endpoint": format!("{revocation_origin}/revoke"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"]
    }))
}

async fn authorize(
    State(p): State<Arc<Provider>>,
    Query(q): Query<HashMap<String, String>>,
) -> Redirect {
    if p.authorization.lock().unwrap().is_empty() {
        assert_eq!(q["client_id"], "dynamic_agent_client");
        assert_eq!(q["agent_name_hint"], "sandb");
    } else {
        assert_eq!(q["client_id"], ISSUED_CLIENT);
        assert!(!q.contains_key("agent_name_hint"));
    }
    assert!(q["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert_eq!(q["scope"], SCOPES);
    assert_eq!(q["resource"], "https://api.openai.com/v1");
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["state"].len() >= 32);
    assert!(q["nonce"].len() >= 32);
    let redirect = openidconnect::url::Url::parse(&q["redirect_uri"]).unwrap();
    assert_eq!(redirect.scheme(), "http");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/auth/callback");
    p.authorization.lock().unwrap().push(q.clone());
    let mut callback = redirect.clone();
    callback
        .query_pairs_mut()
        .append_pair("state", "stray-wrong-state")
        .append_pair("code", "stray-code")
        .append_pair("client_id", ISSUED_CLIENT);
    assert_eq!(reqwest::get(callback).await.unwrap().status(), 400);
    let mut duplicate = redirect.clone();
    duplicate
        .query_pairs_mut()
        .append_pair("state", &q["state"])
        .append_pair("state", &q["state"])
        .append_pair("code", "stray-code");
    assert_eq!(reqwest::get(duplicate).await.unwrap().status(), 400);
    let mut callback = redirect;
    callback
        .query_pairs_mut()
        .append_pair("state", &q["state"])
        .append_pair("scope", SCOPES);
    let fault = *p.fault.lock().unwrap();
    if matches!(fault, Fault::Denied) {
        callback
            .query_pairs_mut()
            .append_pair("error", "access_denied")
            .append_pair("error_description", ACCESS);
    } else {
        callback.query_pairs_mut().append_pair("code", "dummy-code");
        match fault {
            Fault::MissingClient => {}
            Fault::DynamicClient => {
                callback
                    .query_pairs_mut()
                    .append_pair("client_id", "dynamic_agent_client");
            }
            Fault::ClientMismatch => {
                callback
                    .query_pairs_mut()
                    .append_pair("client_id", "oaiapp_wrong_registration");
            }
            _ => {
                callback
                    .query_pairs_mut()
                    .append_pair("client_id", ISSUED_CLIENT);
            }
        }
    }
    Redirect::to(callback.as_str())
}

async fn revoke(
    State(p): State<Arc<Provider>>,
    Form(q): Form<HashMap<String, String>>,
) -> Response {
    assert_eq!(q["client_id"], ISSUED_CLIENT);
    assert_eq!(q["token_type_hint"], "refresh_token");
    assert_eq!(q["token"], *p.refresh_expected.lock().unwrap());
    assert_eq!(q.len(), 3);
    let count = {
        let mut requests = p.revocations.lock().unwrap();
        requests.push(q);
        requests.len()
    };
    let fault = *p.fault.lock().unwrap();
    match fault {
        Fault::RevokeUnavailable => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Fault::RevokeTransient if count < 3 => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Fault::RevokeRedirect => {
            return Redirect::temporary("/revocation-followed").into_response();
        }
        Fault::RevokeNetwork => {
            let body = axum::body::Body::from_stream(futures_util::stream::once(async {
                Err::<Vec<u8>, _>(std::io::Error::other("fixture connection lost"))
            }));
            return (StatusCode::OK, body).into_response();
        }
        Fault::RevokeStalled => {
            p.revoke_started.notify_one();
            p.revoke_release.notified().await;
        }
        _ => {}
    }
    StatusCode::OK.into_response()
}

async fn token(
    State(p): State<Arc<Provider>>,
    Form(q): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if q.get("grant_type").map(String::as_str) == Some("refresh_token") {
        assert_eq!(q["client_id"], ISSUED_CLIENT);
        assert_eq!(q["resource"], "https://api.openai.com/v1");
        assert!(!q.contains_key("scope"));
        assert!(!q.contains_key("client_secret"));
        assert_eq!(q["refresh_token"], *p.refresh_expected.lock().unwrap());
        let number = p.refreshes.fetch_add(1, Ordering::SeqCst) + 1;
        if p.block_refresh.load(Ordering::SeqCst) {
            p.refresh_started.notify_one();
            p.refresh_release.notified().await;
        }
        let fault = *p.fault.lock().unwrap();
        if let Fault::RefreshError(code) = fault {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": code, "error_description": REFRESH})),
            );
        }
        if matches!(fault, Fault::TransientRefresh) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "invalid_grant", "detail": REFRESH})),
            );
        }
        let replacement = format!("dummy-refresh-renewed-{number}");
        *p.refresh_expected.lock().unwrap() = replacement.clone();
        let access = format!("dummy-access-renewed-{number}");
        let mut response = json!({
            "access_token": access,
            "refresh_token": replacement, "token_type": "Bearer",
            "expires_in": p.refresh_lifetime.load(Ordering::SeqCst),
            "earliest_refresh_at": {"opaque": "do-not-interpret"}
        });
        if matches!(fault, Fault::MissingDirectScope) {
            response["scope"] = json!("openid profile email offline_access resource.invoke");
        }
        if matches!(fault, Fault::ExpandedScope) {
            response["scope"] = json!(format!("{SCOPES} unapproved.scope"));
        }
        if matches!(
            fault,
            Fault::RefreshId
                | Fault::RefreshSameNonce
                | Fault::RefreshAuthTime
                | Fault::Nonce
                | Fault::Issuer
                | Fault::Audience
                | Fault::Expired
                | Fault::IdentityMismatch
                | Fault::AccessHash
                | Fault::Signature
                | Fault::Algorithm
        ) {
            response["id_token"] = json!(refresh_id(&p, fault, &access));
        }
        return (StatusCode::OK, Json(response));
    }
    p.exchanges.fetch_add(1, Ordering::SeqCst);
    let auth = p.authorization.lock().unwrap().last().unwrap().clone();
    let fault = *p.fault.lock().unwrap();
    assert_eq!(q["client_id"], ISSUED_CLIENT);
    assert_eq!(q["grant_type"], "authorization_code");
    assert_eq!(q["code"], "dummy-code");
    assert_eq!(q["redirect_uri"], auth["redirect_uri"]);
    assert_eq!(q["resource"], "https://api.openai.com/v1");
    assert!(!q.contains_key("client_secret"));
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(
        q["code_verifier"].clone(),
    ));
    assert_eq!(challenge.as_str(), auth["code_challenge"]);
    if matches!(fault, Fault::InvalidGrant) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "invalid_grant", "error_description": ACCESS
            })),
        );
    }
    let issuer = if matches!(fault, Fault::Issuer) {
        "https://wrong.example.test".into()
    } else {
        p.issuer.clone()
    };
    let audience = if matches!(fault, Fault::Audience) {
        "wrong-client"
    } else {
        ISSUED_CLIENT
    };
    let lifetime = if matches!(fault, Fault::Expired) {
        Duration::minutes(-5)
    } else {
        Duration::minutes(5)
    };
    let subject = match fault {
        Fault::EmptySubject => "",
        Fault::IdentityMismatch => "wrong-subject",
        _ => "dummy-subject",
    };
    let nonce = if matches!(fault, Fault::Nonce) {
        "wrong-nonce".into()
    } else {
        auth["nonce"].clone()
    };
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer).unwrap(),
        vec![Audience::new(audience.into())],
        Utc::now() + lifetime,
        Utc::now(),
        StandardClaims::new(SubjectIdentifier::new(subject.into()))
            .set_email(Some(EndUserEmail::new("dummy@example.test".into()))),
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let alg = if matches!(fault, Fault::Algorithm) {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha512
    } else {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256
    };
    let access = if matches!(fault, Fault::AccessHash) {
        "wrong-access-token"
    } else {
        ACCESS
    };
    let id_token = CoreIdToken::new(
        claims,
        &signing_key(),
        alg,
        Some(&AccessToken::new(access.into())),
        None,
    )
    .unwrap();
    let mut id_token = id_token.to_string();
    if matches!(fault, Fault::Signature) {
        let index = id_token.rfind('.').unwrap() + 1;
        let replacement = if &id_token[index..index + 1] == "A" {
            "B"
        } else {
            "A"
        };
        id_token.replace_range(index..index + 1, replacement);
    }
    *p.issued_id_token.lock().unwrap() = id_token.clone();
    let mut response = json!({
        "access_token": ACCESS, "refresh_token": REFRESH, "id_token": id_token,
        "token_type": "Bearer", "expires_in": p.access_lifetime.load(Ordering::SeqCst), "scope": SCOPES
    });
    if matches!(fault, Fault::MissingDirectScope) {
        response["scope"] = json!("openid profile email offline_access resource.invoke");
    }
    if matches!(fault, Fault::MissingScopes) {
        response.as_object_mut().unwrap().remove("scope");
    }
    (StatusCode::OK, Json(response))
}

fn refresh_id(p: &Provider, fault: Fault, access: &str) -> String {
    let mut claims = json!({
        "iss": p.issuer, "sub": "dummy-subject", "aud": [ISSUED_CLIENT],
        "iat": Utc::now().timestamp(), "exp": (Utc::now() + Duration::minutes(5)).timestamp()
    });
    match fault {
        Fault::Nonce => claims["nonce"] = json!("wrong-nonce"),
        Fault::RefreshSameNonce => {
            claims["nonce"] = json!(p.authorization.lock().unwrap().last().unwrap()["nonce"])
        }
        Fault::Issuer => claims["iss"] = json!("https://wrong.example.test"),
        Fault::Audience => claims["aud"] = json!(["wrong-client"]),
        Fault::Expired => claims["exp"] = json!((Utc::now() - Duration::minutes(5)).timestamp()),
        Fault::IdentityMismatch => claims["sub"] = json!("wrong-subject"),
        Fault::RefreshAuthTime => claims["auth_time"] = json!(Utc::now().timestamp()),
        _ => {}
    }
    let claims: CoreIdTokenClaims = serde_json::from_value(claims).unwrap();
    let alg = if matches!(fault, Fault::Algorithm) {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha512
    } else {
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256
    };
    let access = if matches!(fault, Fault::AccessHash) {
        "wrong-access-token"
    } else {
        access
    };
    let mut token = CoreIdToken::new(
        claims,
        &signing_key(),
        alg,
        Some(&AccessToken::new(access.into())),
        None,
    )
    .unwrap()
    .to_string();
    if matches!(fault, Fault::Signature) {
        let index = token.rfind('.').unwrap() + 1;
        let replacement = if &token[index..index + 1] == "A" {
            "B"
        } else {
            "A"
        };
        token.replace_range(index..index + 1, replacement);
    }
    token
}

#[derive(Clone, Default)]
struct TestKeychain(Arc<MemoryStore>);
impl SecretStore for TestKeychain {
    fn get(&self, a: &str) -> Result<Option<String>, AppError> {
        self.0.get(a)
    }
    fn set(&self, a: &str, s: &str) -> Result<(), AppError> {
        self.0.set(a, s)
    }
    fn delete(&self, a: &str) -> Result<(), AppError> {
        self.0.delete(a)
    }
}

#[tokio::test]
async fn sign_in_validates_and_persists_a_token_free_session_view() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    assert!(!session.status(&vault).unwrap().signed_in);
    let view = tokio::time::timeout(std::time::Duration::from_secs(10), session.sign_in(&vault))
        .await
        .unwrap()
        .unwrap();
    fixture.finish_browser().await;
    let expected = json!({
        "signedIn": true, "accountLabel": "dummy@example.test", "usageEnabled": true
    });
    assert_eq!(serde_json::to_value(view).unwrap(), expected);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert_eq!(
        serde_json::to_value(OAuthSession::default().status(&restarted).unwrap()).unwrap(),
        expected,
    );
    let file = std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap();
    assert!(!file.contains(ACCESS));
    assert!(!file.contains(REFRESH));
    assert!(!file.contains("dummy@example.test"));
    let auth = fixture
        .provider
        .authorization
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let mut replay = openidconnect::url::Url::parse(&auth["redirect_uri"]).unwrap();
    replay
        .query_pairs_mut()
        .append_pair("state", &auth["state"])
        .append_pair("code", "dummy-code")
        .append_pair("client_id", ISSUED_CLIENT);
    assert!(
        reqwest::get(replay).await.is_err(),
        "completed listener must be closed"
    );
    assert_eq!(fixture.provider.exchanges.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unvalidated_oauth_results_never_create_an_active_session() {
    for fault in [
        Fault::Nonce,
        Fault::Issuer,
        Fault::Audience,
        Fault::Expired,
        Fault::EmptySubject,
        Fault::AccessHash,
        Fault::Signature,
        Fault::Algorithm,
        Fault::JwksRedirect,
        Fault::UntrustedJwks,
        Fault::MissingClient,
        Fault::DynamicClient,
        Fault::Denied,
    ] {
        let fixture = Fixture::start().await;
        fixture.fault(fault);
        let secrets = MemoryStore::new();
        let session = fixture.session();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            session.sign_in(&secrets),
        )
        .await
        .unwrap();
        fixture.finish_browser().await;
        let error = result.expect_err(&format!("accepted {fault:?}"));
        let error = serde_json::to_string(&error).unwrap();
        assert!(!error.contains(ACCESS));
        assert!(!error.contains("http://"));
        assert_eq!(
            session.status(&secrets).unwrap(),
            SessionView {
                signed_in: false,
                account_label: None,
                usage_enabled: false
            },
            "{fault:?}",
        );
        if matches!(
            fault,
            Fault::MissingClient | Fault::DynamicClient | Fault::Denied
        ) {
            assert_eq!(fixture.provider.exchanges.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn identity_without_granted_usage_scope_remains_signed_in_but_disabled() {
    for fault in [Fault::MissingDirectScope, Fault::MissingScopes] {
        let fixture = Fixture::start().await;
        fixture.fault(fault);
        let secrets = MemoryStore::new();
        let session = fixture.session();
        let view = session.sign_in(&secrets).await.unwrap();
        fixture.finish_browser().await;
        assert_eq!(
            view,
            SessionView {
                signed_in: true,
                account_label: Some("dummy@example.test".into()),
                usage_enabled: false,
            }
        );
        assert_eq!(OAuthSession::default().status(&secrets).unwrap(), view);
    }
}

#[tokio::test]
async fn failed_reauthorization_preserves_the_active_account_and_credentials() {
    let fixture = Fixture::start().await;
    let secrets = MemoryStore::new();
    let session = fixture.session();
    let original = session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    for fault in [
        Fault::ClientMismatch,
        Fault::IdentityMismatch,
        Fault::Nonce,
        Fault::InvalidGrant,
        Fault::Denied,
    ] {
        fixture.fault(fault);
        assert!(
            session.sign_in(&secrets).await.is_err(),
            "accepted {fault:?}"
        );
        fixture.finish_browser().await;
        assert_eq!(session.status(&secrets).unwrap(), original);
    }
    fixture.fault(Fault::MissingClient);
    assert_eq!(session.sign_in(&secrets).await.unwrap(), original);
    fixture.finish_browser().await;
    let requests = fixture.provider.authorization.lock().unwrap();
    for pair in requests.windows(2) {
        assert_eq!(pair[0]["ext_agent_host_id"], pair[1]["ext_agent_host_id"]);
        for key in ["state", "nonce", "code_challenge"] {
            assert_ne!(pair[0][key], pair[1][key]);
        }
        assert!(pair[1].contains_key("id_token_hint"));
        assert_eq!(pair[1]["login_hint"], "dummy@example.test");
    }
}

#[tokio::test]
async fn invalid_grant_retains_issued_registration_for_fresh_sign_in_after_restart() {
    let fixture = Fixture::start().await;
    fixture.fault(Fault::InvalidGrant);
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let first = fixture.session();
    assert!(first.sign_in(&vault).await.is_err());
    fixture.finish_browser().await;
    assert!(!first.status(&vault).unwrap().signed_in);
    drop(vault);
    fixture.fault(Fault::MissingClient);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert!(
        fixture
            .session()
            .sign_in(&restarted)
            .await
            .unwrap()
            .signed_in
    );
    fixture.finish_browser().await;
    let requests = fixture.provider.authorization.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["ext_agent_host_id"],
        requests[1]["ext_agent_host_id"]
    );
    assert_ne!(requests[0]["state"], requests[1]["state"]);
    assert_ne!(requests[0]["nonce"], requests[1]["nonce"]);
    assert_ne!(requests[0]["code_challenge"], requests[1]["code_challenge"]);
    assert!(!requests[1].contains_key("id_token_hint"));
}

#[tokio::test]
async fn stalled_sign_in_times_out_closes_listener_and_releases_single_attempt_lock() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let session = OAuthSession {
        browser: Some(Arc::new(move |url| {
            send.send(url.to_owned()).unwrap();
            Ok(())
        })),
        timeout: Some(std::time::Duration::from_millis(50)),
        ..Default::default()
    };
    let secrets = MemoryStore::new();
    let login = session.sign_in(&secrets);
    tokio::pin!(login);
    let authorization = tokio::select! {
        url = receive.recv() => url.unwrap(),
        _ = &mut login => panic!("login ended before browser launch"),
    };
    assert_eq!(
        session.sign_in(&secrets).await.unwrap_err().code,
        ErrorCode::InvalidInput
    );
    assert_eq!(login.await.unwrap_err().code, ErrorCode::Timeout);
    assert!(!session.status(&secrets).unwrap().signed_in);
    let url = openidconnect::url::Url::parse(&authorization).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .unwrap()
            .get(&query["redirect_uri"])
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        session.sign_in(&secrets).await.unwrap_err().code,
        ErrorCode::Timeout
    );
    assert!(
        receive.recv().await.is_some(),
        "a new attempt can launch its browser"
    );
}

struct RejectPendingWrite {
    inner: MemoryStore,
    writes: AtomicUsize,
}
impl SecretStore for RejectPendingWrite {
    fn get(&self, a: &str) -> Result<Option<String>, AppError> {
        self.inner.get(a)
    }
    fn set(&self, a: &str, s: &str) -> Result<(), AppError> {
        if self.writes.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(AppError::new(
                ErrorCode::SecretStore,
                "fixture vault is locked",
            ));
        }
        self.inner.set(a, s)
    }
    fn delete(&self, a: &str) -> Result<(), AppError> {
        self.inner.delete(a)
    }
}

#[tokio::test]
async fn registration_must_be_protected_before_code_exchange() {
    let fixture = Fixture::start().await;
    let secrets = RejectPendingWrite {
        inner: MemoryStore::new(),
        writes: AtomicUsize::new(0),
    };
    let session = fixture.session();
    assert_eq!(
        session.sign_in(&secrets).await.unwrap_err().code,
        ErrorCode::SecretStore
    );
    fixture.finish_browser().await;
    assert_eq!(fixture.provider.exchanges.load(Ordering::SeqCst), 0);
    assert!(!session.status(&secrets).unwrap().signed_in);
}

#[tokio::test]
async fn cancel_sign_in_closes_listener_promptly_without_destroying_active_session() {
    let fixture = Fixture::start().await;
    let secrets = MemoryStore::new();
    let mut session = fixture.session();
    let original = session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    let active_lease = session
        .access_token(&secrets, &CancellationToken::new())
        .await
        .unwrap();
    let browser = session.browser.take();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    session.browser = Some(Arc::new(move |url| {
        send.send(url.to_owned()).unwrap();
        Ok(())
    }));
    session.timeout = Some(std::time::Duration::from_secs(2));
    {
        let login = session.sign_in(&secrets);
        tokio::pin!(login);
        let authorization = tokio::select! {
            url = receive.recv() => url.unwrap(),
            _ = &mut login => panic!("login ended before browser launch"),
        };
        session.cancel_sign_in();
        let result = tokio::time::timeout(std::time::Duration::from_millis(500), login)
            .await
            .expect("cancel must be prompt");
        assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
        assert_eq!(session.status(&secrets).unwrap(), original);
        assert!(!active_lease.cancellation().is_cancelled());
        assert_eq!(active_lease.token().unwrap(), ACCESS);
        let url = openidconnect::url::Url::parse(&authorization).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert!(reqwest::get(&query["redirect_uri"]).await.is_err());
    }
    session.cancel_sign_in();
    session.browser = browser;
    assert_eq!(session.sign_in(&secrets).await.unwrap(), original);
    fixture.finish_browser().await;
}

#[tokio::test]
async fn concurrent_access_renews_once_and_resumes_the_rotated_token_after_restart() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    session.sign_in(&vault).await.unwrap();
    fixture.finish_browser().await;
    let cancel = CancellationToken::new();
    let (first, second) = tokio::join!(
        session.access_token(&vault, &cancel),
        session.access_token(&vault, &cancel)
    );
    assert_eq!(first.unwrap().token().unwrap(), "dummy-access-renewed-1");
    assert_eq!(second.unwrap().token().unwrap(), "dummy-access-renewed-1");
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 1);
    drop(vault);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert_eq!(
        fixture
            .session()
            .access_token(&restarted, &cancel)
            .await
            .unwrap()
            .token()
            .unwrap(),
        "dummy-access-renewed-1"
    );
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 1);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        session
            .access_token(&restarted, &cancelled)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_refresh_clears_tokens_but_transient_and_invalid_client_preserve_them() {
    for (fault, terminal) in [
        (Fault::TransientRefresh, false),
        (Fault::RefreshError("invalid_client"), false),
        (Fault::RefreshError("invalid_grant"), true),
        (Fault::RefreshError("invalid_refresh_token"), true),
        (Fault::RefreshError("token_expired"), true),
        (Fault::RefreshError("refresh_token_expired"), true),
        (Fault::RefreshError("refresh_token_invalidated"), true),
        (Fault::RefreshError("refresh_token_reused"), true),
    ] {
        let fixture = Fixture::start().await;
        fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
        let secrets = MemoryStore::new();
        let session = fixture.session();
        session.sign_in(&secrets).await.unwrap();
        fixture.finish_browser().await;
        fixture.fault(fault);
        let cancel = CancellationToken::new();
        let error = session
            .access_token(&secrets, &cancel)
            .await
            .err()
            .expect("must reject refresh error");
        assert!(!serde_json::to_string(&error).unwrap().contains(REFRESH));
        assert_eq!(
            session.status(&secrets).unwrap().signed_in,
            !terminal,
            "{fault:?}"
        );
        fixture.fault(Fault::None);
        if terminal {
            assert!(session.access_token(&secrets, &cancel).await.is_err());
            session.sign_in(&secrets).await.unwrap();
            fixture.finish_browser().await;
            assert!(
                !fixture
                    .provider
                    .authorization
                    .lock()
                    .unwrap()
                    .last()
                    .unwrap()
                    .contains_key("id_token_hint")
            );
        } else {
            assert!(
                session.access_token(&secrets, &cancel).await.is_ok(),
                "preserved refresh token must still work"
            );
        }
    }
}

#[tokio::test]
async fn renewed_registration_uses_latest_refresh_token_after_restart() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
    fixture
        .provider
        .refresh_lifetime
        .store(30, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    session.sign_in(&vault).await.unwrap();
    fixture.finish_browser().await;
    let cancel = CancellationToken::new();
    assert_eq!(
        session
            .access_token(&vault, &cancel)
            .await
            .unwrap()
            .token()
            .unwrap(),
        "dummy-access-renewed-1"
    );
    drop(vault);
    fixture
        .provider
        .refresh_lifetime
        .store(3600, Ordering::SeqCst);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert_eq!(
        fixture
            .session()
            .access_token(&restarted, &cancel)
            .await
            .unwrap()
            .token()
            .unwrap(),
        "dummy-access-renewed-2"
    );
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn refresh_id_token_enforces_original_identity_nonce_and_oidc_context() {
    for (fault, valid) in [
        (Fault::RefreshId, true),
        (Fault::RefreshSameNonce, true),
        (Fault::Nonce, false),
        (Fault::Issuer, false),
        (Fault::Audience, false),
        (Fault::Expired, false),
        (Fault::IdentityMismatch, false),
        (Fault::AccessHash, false),
        (Fault::Signature, false),
        (Fault::Algorithm, false),
        (Fault::RefreshAuthTime, false),
    ] {
        let fixture = Fixture::start().await;
        fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
        let secrets = MemoryStore::new();
        let session = fixture.session();
        session.sign_in(&secrets).await.unwrap();
        fixture.finish_browser().await;
        fixture.fault(fault);
        let result = session
            .access_token(&secrets, &CancellationToken::new())
            .await;
        assert_eq!(result.is_ok(), valid, "{fault:?}");
        assert!(
            session.status(&secrets).unwrap().signed_in,
            "invalid refresh must not replace active credentials"
        );
    }
}

#[tokio::test]
async fn inference_access_requires_granted_scope_and_never_escalates_it_on_refresh() {
    for fault in [Fault::MissingDirectScope, Fault::ExpandedScope] {
        let fixture = Fixture::start().await;
        fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
        let secrets = MemoryStore::new();
        let session = fixture.session();
        session.sign_in(&secrets).await.unwrap();
        fixture.finish_browser().await;
        fixture.fault(fault);
        assert!(
            session
                .access_token(&secrets, &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(session.status(&secrets).unwrap().signed_in);
        if matches!(fault, Fault::MissingDirectScope) {
            assert!(!session.status(&secrets).unwrap().usage_enabled);
            let previous = fixture.provider.refreshes.load(Ordering::SeqCst);
            assert!(
                fixture
                    .session()
                    .access_token(&secrets, &CancellationToken::new())
                    .await
                    .is_err()
            );
            assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), previous);
        }
    }
    let fixture = Fixture::start().await;
    fixture.fault(Fault::MissingDirectScope);
    let secrets = MemoryStore::new();
    let session = fixture.session();
    session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    assert!(
        session
            .access_token(&secrets, &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn logout_revokes_latest_rotation_cancels_lease_and_retains_registration_after_restart() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    session.sign_in(&vault).await.unwrap();
    fixture.finish_browser().await;
    let lease = session
        .access_token(&vault, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(lease.token().unwrap(), "dummy-access-renewed-1");
    let view = session.logout(&vault).await.unwrap();
    assert!(!view.revocation_unconfirmed);
    assert!(
        lease.cancellation().is_cancelled(),
        "logout must abort OAuth requests using this session"
    );
    assert_eq!(lease.token().err().unwrap().code, ErrorCode::Cancelled);
    assert!(!session.status(&vault).unwrap().signed_in);
    assert!(
        session
            .access_token(&vault, &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(fixture.provider.revocations.lock().unwrap().len(), 1);
    drop(vault);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    let next = fixture.session();
    assert!(!next.status(&restarted).unwrap().signed_in);
    next.sign_in(&restarted).await.unwrap();
    fixture.finish_browser().await;
    assert!(
        !fixture
            .provider
            .authorization
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains_key("id_token_hint")
    );
    assert!(lease.cancellation().is_cancelled());
}

#[tokio::test]
async fn revocation_retries_are_bounded_and_failure_warns_without_retaining_tokens() {
    for (fault, calls, unconfirmed) in [
        (Fault::RevokeTransient, 3, false),
        (Fault::RevokeUnavailable, 3, true),
        (Fault::RevokeNetwork, 3, true),
        (Fault::RevokeRedirect, 1, true),
        (Fault::RevokeUntrusted, 0, true),
        (Fault::RevokeIssuer, 0, true),
    ] {
        let fixture = Fixture::start().await;
        let secrets = MemoryStore::new();
        let session = fixture.session();
        session.sign_in(&secrets).await.unwrap();
        fixture.finish_browser().await;
        let lease = session
            .access_token(&secrets, &CancellationToken::new())
            .await
            .unwrap();
        fixture.fault(fault);
        let view =
            tokio::time::timeout(std::time::Duration::from_secs(5), session.logout(&secrets))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(view.revocation_unconfirmed, unconfirmed, "{fault:?}");
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            json!({"revocationUnconfirmed": unconfirmed})
        );
        assert_eq!(
            fixture.provider.revocations.lock().unwrap().len(),
            calls,
            "{fault:?}"
        );
        assert!(lease.cancellation().is_cancelled());
        assert!(!fixture.session().status(&secrets).unwrap().signed_in);
        assert!(
            fixture
                .session()
                .access_token(&secrets, &CancellationToken::new())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn logout_cancels_pending_login_without_resurrecting_a_session() {
    let fixture = Fixture::start().await;
    let secrets = MemoryStore::new();
    let mut session = fixture.session();
    session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    let lease = session
        .access_token(&secrets, &CancellationToken::new())
        .await
        .unwrap();
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    session.browser = Some(Arc::new(move |url| {
        send.send(url.to_owned()).unwrap();
        Ok(())
    }));
    let login = session.sign_in(&secrets);
    tokio::pin!(login);
    let authorization = tokio::select! {
        url = receive.recv() => url.unwrap(),
        _ = &mut login => panic!("login ended before browser launch"),
    };
    let logout = session.logout(&secrets);
    tokio::pin!(logout);
    assert!(futures_util::poll!(&mut logout).is_pending());
    assert!(lease.cancellation().is_cancelled());
    assert!(!session.status(&secrets).unwrap().signed_in);
    let (login, logout) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(login, logout)
    })
    .await
    .unwrap();
    assert_eq!(login.unwrap_err().code, ErrorCode::Cancelled);
    assert!(!logout.unwrap().revocation_unconfirmed);
    assert!(!fixture.session().status(&secrets).unwrap().signed_in);
    let url = openidconnect::url::Url::parse(&authorization).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert!(reqwest::get(&query["redirect_uri"]).await.is_err());
}

#[tokio::test]
async fn logout_waits_for_inflight_rotation_and_revokes_it_without_resurrection() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
    fixture.provider.block_refresh.store(true, Ordering::SeqCst);
    let secrets = MemoryStore::new();
    let session = fixture.session();
    session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    let cancel = CancellationToken::new();
    let access = session.access_token(&secrets, &cancel);
    tokio::pin!(access);
    tokio::select! {
        _ = fixture.provider.refresh_started.notified() => {},
        _ = &mut access => panic!("refresh ended before fixture released it"),
    }
    let logout = session.logout(&secrets);
    tokio::pin!(logout);
    assert!(futures_util::poll!(&mut logout).is_pending());
    assert!(!session.status(&secrets).unwrap().signed_in);
    assert!(session.access_token(&secrets, &cancel).await.is_err());
    assert!(session.sign_in(&secrets).await.is_err());
    fixture.provider.refresh_release.notify_one();
    let (access, logout) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(access, logout)
    })
    .await
    .unwrap();
    assert_eq!(access.err().unwrap().code, ErrorCode::Cancelled);
    assert!(!logout.unwrap().revocation_unconfirmed);
    assert_eq!(
        fixture.provider.revocations.lock().unwrap()[0]["token"],
        "dummy-refresh-renewed-1"
    );
    assert!(!fixture.session().status(&secrets).unwrap().signed_in);
}

#[tokio::test]
async fn cancelled_request_finishes_rotation_but_never_receives_an_inference_token() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(30, Ordering::SeqCst);
    fixture.provider.block_refresh.store(true, Ordering::SeqCst);
    let secrets = MemoryStore::new();
    let session = fixture.session();
    session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    let cancel = CancellationToken::new();
    let access = session.access_token(&secrets, &cancel);
    tokio::pin!(access);
    tokio::select! {
        _ = fixture.provider.refresh_started.notified() => {},
        _ = &mut access => panic!("refresh ended before fixture released it"),
    }
    cancel.cancel();
    fixture.provider.refresh_release.notify_one();
    assert_eq!(access.await.err().unwrap().code, ErrorCode::Cancelled);
    assert_eq!(
        session
            .access_token(&secrets, &CancellationToken::new())
            .await
            .unwrap()
            .token()
            .unwrap(),
        "dummy-access-renewed-1"
    );
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_logout_during_revocation_leaves_durable_tokens_cleared() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    session.sign_in(&vault).await.unwrap();
    fixture.finish_browser().await;
    let lease = session
        .access_token(&vault, &CancellationToken::new())
        .await
        .unwrap();
    fixture.fault(Fault::RevokeStalled);
    {
        let logout = session.logout(&vault);
        tokio::pin!(logout);
        tokio::select! {
            _ = fixture.provider.revoke_started.notified() => {},
            _ = &mut logout => panic!("revocation unexpectedly finished"),
        }
        assert!(lease.cancellation().is_cancelled());
        assert!(!session.status(&vault).unwrap().signed_in);
    }
    fixture.provider.revoke_release.notify_one();
    drop(vault);
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert!(!fixture.session().status(&restarted).unwrap().signed_in);
    fixture.fault(Fault::None);
    session.sign_in(&restarted).await.unwrap();
    fixture.finish_browser().await;
}

#[tokio::test]
async fn expired_renewable_session_still_reports_plan_permission_after_restart() {
    let fixture = Fixture::start().await;
    fixture.provider.access_lifetime.store(1, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let keychain = TestKeychain::default();
    let vault = VaultStore::new(dir.path(), Box::new(keychain.clone()));
    let session = fixture.session();
    session.sign_in(&vault).await.unwrap();
    fixture.finish_browser().await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let restarted = VaultStore::new(dir.path(), Box::new(keychain));
    assert!(
        session.status(&restarted).unwrap().usage_enabled,
        "expiry must not look like missing plan permission"
    );
    assert!(
        session
            .access_token(&restarted, &CancellationToken::new())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn explicit_enable_usage_requests_consent_but_ordinary_sign_in_does_not() {
    let fixture = Fixture::start().await;
    fixture.fault(Fault::MissingDirectScope);
    let secrets = MemoryStore::new();
    let session = fixture.session();
    assert!(!session.sign_in(&secrets).await.unwrap().usage_enabled);
    fixture.finish_browser().await;
    fixture.fault(Fault::None);
    assert!(
        session
            .enable_plan_usage(&secrets)
            .await
            .unwrap()
            .usage_enabled
    );
    fixture.finish_browser().await;
    session.sign_in(&secrets).await.unwrap();
    fixture.finish_browser().await;
    let requests = fixture.provider.authorization.lock().unwrap();
    assert!(requests[0].get("prompt").is_none());
    assert_eq!(
        requests[1].get("prompt").map(String::as_str),
        Some("consent")
    );
    assert!(requests[1].get("force_reconsent").is_none());
    assert_eq!(requests[1]["scope"], SCOPES);
    assert_eq!(requests[1]["client_id"], ISSUED_CLIENT);
    assert!(requests[2].get("prompt").is_none());
}
