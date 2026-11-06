//! AI settings in `settings.json`, the API key in the secret store, and the settings test.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::ai::client::{AiClient, Api};
use crate::core::ai::responses;
use crate::core::error::{AppError, ErrorCode};
use crate::core::state::AppState;

/// Secret store account of the AI API key.
pub const AI_KEY_ACCOUNT: &str = "ai:api_key";
pub const MAX_ROWS_LIMIT: u32 = 10_000;
pub const TIMEOUT_SEC_LIMIT: u64 = 300;
/// Model ids put in the `model_not_found` detail.
const LISTED_MODELS: usize = 20;

/// What a query tool result may send to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// Column names, row count and the rows.
    #[default]
    Results,
    /// Column names and row count only; row values never leave the app.
    Schema,
}

/// Missing fields take their default, so older or partial files still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSettings {
    /// `chatgpt` uses OAuth Responses; `anthropic` and `openai` use API-key transports.
    /// Older provider labels (`ollama`, `custom`, …) load as `openai`.
    pub provider: String,
    /// API root, e.g. `http://localhost:11434/v1`. No trailing `/`.
    pub base_url: String,
    pub model: String,
    pub privacy: Privacy,
    /// Row cap of one AI read.
    pub max_rows: u32,
    /// Time limit of one AI read, in seconds.
    pub timeout_sec: u64,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: "llama3.1:8b".into(),
            privacy: Privacy::Results,
            max_rows: 500,
            timeout_sec: 15,
        }
    }
}

/// The settings as the UI sees them. The key itself never leaves the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsView {
    #[serde(flatten)]
    pub settings: AiSettings,
    pub has_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTestResult {
    pub latency_ms: u64,
    pub model_count: usize,
}

impl AiSettings {
    pub fn api(&self) -> Api {
        match self.provider.as_str() {
            "anthropic" => Api::Anthropic,
            "chatgpt" => Api::ChatGpt,
            _ => Api::OpenAi,
        }
    }

    /// Trims the text fields and checks the limits. The base URL loses its trailing `/`.
    /// Unknown providers become `openai`; `chatgpt` never uses the saved base URL.
    pub fn normalized(mut self) -> Result<Self, AppError> {
        self.provider = match self.provider.trim() {
            "anthropic" => "anthropic",
            "chatgpt" => "chatgpt",
            _ => "openai",
        }
        .to_string();
        self.model = self.model.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if self.provider != "chatgpt"
            && !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://"))
        {
            return Err(AppError::invalid(
                "The base URL must start with http:// or https://",
            ));
        }
        if !(1..=MAX_ROWS_LIMIT).contains(&self.max_rows) {
            return Err(AppError::invalid(format!(
                "Max rows must be between 1 and {MAX_ROWS_LIMIT}"
            )));
        }
        if !(1..=TIMEOUT_SEC_LIMIT).contains(&self.timeout_sec) {
            return Err(AppError::invalid(format!(
                "The time limit must be between 1 and {TIMEOUT_SEC_LIMIT} seconds"
            )));
        }
        Ok(self)
    }
}

pub fn load(state: &AppState) -> Result<AiSettings, AppError> {
    Ok(state.store.load_settings()?.ai)
}

/// The stored key; `None` when there is none or it is empty.
pub fn stored_key(state: &AppState) -> Result<Option<String>, AppError> {
    Ok(state.secrets.get(AI_KEY_ACCOUNT)?.filter(|k| !k.is_empty()))
}

pub fn get(state: &AppState) -> Result<AiSettingsView, AppError> {
    Ok(AiSettingsView {
        settings: load(state)?,
        has_key: stored_key(state)?.is_some(),
    })
}

pub fn save(state: &AppState, settings: AiSettings) -> Result<AiSettingsView, AppError> {
    let settings = settings.normalized()?;
    let mut file = state.store.load_settings()?;
    file.ai = settings;
    state.store.save_settings(&file)?;
    get(state)
}

/// `None` or an empty key deletes the stored key.
pub fn set_key(state: &AppState, key: Option<String>) -> Result<(), AppError> {
    match key.as_deref().map(str::trim) {
        None | Some("") => state.secrets.delete(AI_KEY_ACCOUNT),
        Some(k) => state.secrets.set(AI_KEY_ACCOUNT, k),
    }
}

/// `key`: `None` uses the stored key, `Some("")` sends no key.
fn resolve_key(state: &AppState, key: Option<String>) -> Result<Option<String>, AppError> {
    match key {
        None => stored_key(state),
        Some(k) => Ok(Some(k.trim().to_string()).filter(|k| !k.is_empty())),
    }
}

pub(crate) async fn client(
    state: &AppState,
    settings: &AiSettings,
    key: Option<String>,
    cancel: &CancellationToken,
) -> Result<AiClient, AppError> {
    if settings.api() == Api::ChatGpt {
        return Ok(chatgpt_client(state, cancel).await?.with_redaction(key));
    }
    AiClient::new(settings.api(), &settings.base_url, resolve_key(state, key)?)
}

async fn chatgpt_client(
    state: &AppState,
    cancel: &CancellationToken,
) -> Result<AiClient, AppError> {
    let lease = state
        .chatgpt
        .access_token(state.secrets.as_ref(), cancel)
        .await?;
    let client = AiClient::chatgpt(lease)?.with_redaction(stored_key(state)?);
    #[cfg(test)]
    let client = match &state.chatgpt.inference_base {
        Some(base) => client.with_test_base(base),
        None => client,
    };
    Ok(client)
}

/// Account catalog retains display labels and server order for the forthcoming picker.
pub async fn chatgpt_catalog(state: &AppState) -> Result<Vec<responses::Model>, AppError> {
    chatgpt_client(state, &CancellationToken::new())
        .await?
        .chatgpt_models()
        .await
}

/// Model ids of `settings` (saved or not), sorted, without duplicates. The model field is not
/// checked. `key`: as in [`test`].
pub async fn models(
    state: &AppState,
    settings: AiSettings,
    key: Option<String>,
) -> Result<Vec<String>, AppError> {
    let settings = settings.normalized()?;
    if settings.api() == Api::ChatGpt {
        return client(state, &settings, key, &CancellationToken::new())
            .await?
            .list_models()
            .await;
    }
    let client = client(state, &settings, key, &CancellationToken::new()).await?;
    let mut models = client.list_models().await?;
    models.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    models.dedup();
    Ok(models)
}

/// Tests `settings` (saved or not): the server must answer `GET /models` and list the model.
/// `key`: `None` uses the stored key, `Some("")` sends no key.
pub async fn test(
    state: &AppState,
    settings: AiSettings,
    key: Option<String>,
) -> Result<AiTestResult, AppError> {
    let settings = settings.normalized()?;
    if settings.model.is_empty() {
        return Err(AppError::invalid("Enter a model name"));
    }
    let client = client(state, &settings, key, &CancellationToken::new()).await?;
    let started = Instant::now();
    let models = client.list_models().await?;
    let latency_ms = started.elapsed().as_millis() as u64;
    check_model(&settings.model, &models)?;
    Ok(AiTestResult {
        latency_ms,
        model_count: models.len(),
    })
}

/// `model_not_found` unless `model` is one of `models`. Ollama lists `llama3` as
/// `llama3:latest` and accepts either name, so that entry counts too.
pub fn check_model(model: &str, models: &[String]) -> Result<(), AppError> {
    if models
        .iter()
        .any(|m| m == model || m.strip_suffix(":latest") == Some(model))
    {
        return Ok(());
    }
    let count = models.len();
    let noun = if count == 1 { "model" } else { "models" };
    let err = AppError::new(
        ErrorCode::ModelNotFound,
        format!("Model \"{model}\" is not on this server. The server lists {count} {noun}."),
    );
    if models.is_empty() {
        return Err(err);
    }
    let shown: Vec<&str> = models
        .iter()
        .take(LISTED_MODELS)
        .map(String::as_str)
        .collect();
    Err(err.with_detail(shown.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::secrets::MemoryStore;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        (dir, st)
    }

    #[test]
    fn normalizes_url_and_model() {
        let s = AiSettings {
            base_url: "  https://api.example.com/v1//  ".into(),
            model: " gpt-x ".into(),
            ..Default::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(s.base_url, "https://api.example.com/v1");
        assert_eq!(s.model, "gpt-x");
    }

    #[test]
    fn rejects_bad_values() {
        let bad = [
            AiSettings {
                base_url: "ftp://x".into(),
                ..Default::default()
            },
            AiSettings {
                base_url: "localhost:11434".into(),
                ..Default::default()
            },
            AiSettings {
                max_rows: 0,
                ..Default::default()
            },
            AiSettings {
                max_rows: MAX_ROWS_LIMIT + 1,
                ..Default::default()
            },
            AiSettings {
                timeout_sec: 0,
                ..Default::default()
            },
            AiSettings {
                timeout_sec: TIMEOUT_SEC_LIMIT + 1,
                ..Default::default()
            },
        ];
        for s in bad {
            let e = s.clone().normalized().unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{s:?}");
        }
        let edge = AiSettings {
            max_rows: MAX_ROWS_LIMIT,
            timeout_sec: TIMEOUT_SEC_LIMIT,
            ..Default::default()
        };
        assert!(edge.normalized().is_ok());
    }

    #[test]
    fn partial_json_takes_defaults() {
        let s: AiSettings = serde_json::from_str(r#"{"model": "m", "privacy": "schema"}"#).unwrap();
        assert_eq!(s.model, "m");
        assert_eq!(s.privacy, Privacy::Schema);
        assert_eq!(s.base_url, AiSettings::default().base_url);
        assert_eq!(s.max_rows, 500);
    }

    #[test]
    fn view_is_flat_camel_case_without_key() {
        let (_d, st) = state();
        set_key(&st, Some("sk-secret".into())).unwrap();
        let v = serde_json::to_value(get(&st).unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "provider": "openai",
                "baseUrl": "http://localhost:11434/v1",
                "model": "llama3.1:8b",
                "privacy": "results",
                "maxRows": 500,
                "timeoutSec": 15,
                "hasKey": true,
            })
        );
        assert!(!v.to_string().contains("sk-secret"));
    }

    #[test]
    fn save_persists_and_key_can_be_cleared() {
        let (dir, st) = state();
        let saved = save(
            &st,
            AiSettings {
                base_url: "http://127.0.0.1:1/v1/".into(),
                model: "m1".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(saved.settings.base_url, "http://127.0.0.1:1/v1");
        assert!(!saved.has_key);
        // A fresh state on the same dir reads the same file.
        let st2 = AppState::new(dir.path(), Box::new(MemoryStore::new())).unwrap();
        assert_eq!(load(&st2).unwrap().model, "m1");

        set_key(&st, Some("k".into())).unwrap();
        assert!(get(&st).unwrap().has_key);
        set_key(&st, Some(String::new())).unwrap();
        assert!(!get(&st).unwrap().has_key);
        set_key(&st, Some("k".into())).unwrap();
        set_key(&st, None).unwrap();
        assert!(!get(&st).unwrap().has_key);
    }

    #[test]
    fn save_rejects_invalid_and_keeps_file() {
        let (_d, st) = state();
        let e = save(
            &st,
            AiSettings {
                max_rows: 0,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert_eq!(load(&st).unwrap(), AiSettings::default());
    }

    #[test]
    fn model_check() {
        let models: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
        assert!(check_model("m3", &models).is_ok());
        let e = check_model("nope", &models).unwrap_err();
        assert_eq!(e.code, ErrorCode::ModelNotFound);
        assert_eq!(
            e.message,
            "Model \"nope\" is not on this server. The server lists 25 models."
        );
        let detail = e.detail.unwrap();
        assert_eq!(detail.split(", ").count(), 20);
        assert!(detail.starts_with("m0, m1"));
        let e = check_model("x", &["only".into()]).unwrap_err();
        assert_eq!(
            e.message,
            "Model \"x\" is not on this server. The server lists 1 model."
        );
        let ollama = ["llama3:latest".to_string(), "qwen2.5:7b".to_string()];
        assert!(check_model("llama3", &ollama).is_ok());
        assert!(check_model("llama3:latest", &ollama).is_ok());
        assert!(check_model("qwen2.5:7b", &ollama).is_ok());
        assert!(check_model("qwen2.5", &ollama).is_err());
        assert!(check_model("llama", &ollama).is_err());
    }

    #[tokio::test]
    async fn test_rejects_empty_model() {
        let (_d, st) = state();
        let e = test(
            &st,
            AiSettings {
                model: "  ".into(),
                ..Default::default()
            },
            Some(String::new()),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
    }
}
