//! AI settings in `settings.json`, the API key in the secret store, and the settings test.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::core::ai::client::{AiClient, Provider};
use crate::core::error::{AppError, ErrorCode};
use crate::core::result::{DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT, ReadLimits};
use crate::core::secrets::AI_KEY_ACCOUNT;
use crate::core::state::AppState;

/// The `settings.json` section the AI settings live in.
const SECTION: &str = "ai";
pub const MAX_ROWS_LIMIT: u32 = 10_000;
pub const TIMEOUT_SEC_LIMIT: u64 = 300;
/// API root of a fresh install (a local Ollama); the settings page shows it before the first load.
pub const DEFAULT_AI_BASE_URL: &str = "http://localhost:11434/v1";
/// Model of a fresh install.
pub const DEFAULT_AI_MODEL: &str = "llama3.1:8b";
/// Model ids put in the `model_not_found` detail.
const LISTED_MODELS: usize = 20;

/// What a query tool result may send to the model.
#[cfg_attr(test, derive(ts_rs::TS))]
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
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AiSettings {
    pub provider: Provider,
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
            provider: Provider::OpenAi,
            base_url: DEFAULT_AI_BASE_URL.into(),
            model: DEFAULT_AI_MODEL.into(),
            privacy: Privacy::Results,
            max_rows: DEFAULT_MAX_ROWS as u32,
            timeout_sec: DEFAULT_TIMEOUT.as_secs(),
        }
    }
}

/// The settings as the UI sees them. The key itself never leaves the backend.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsView {
    #[serde(flatten)]
    pub settings: AiSettings,
    pub has_key: bool,
}

#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTestResult {
    pub latency_ms: u64,
    pub model_count: usize,
}

impl AiSettings {
    pub fn read_limits(&self) -> ReadLimits {
        ReadLimits {
            max_rows: self.max_rows as usize,
            timeout: std::time::Duration::from_secs(self.timeout_sec),
        }
    }

    pub fn normalized(mut self) -> Result<Self, AppError> {
        self.model = self.model.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
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
    state.store.load_section(SECTION)
}

/// The key one AI call sends: `None` (the chat) reads the stored key, `Some` (the settings
/// page) is the caller's, trimmed; an empty one means no key.
fn key_for(state: &AppState, key: Option<String>) -> Result<Option<String>, AppError> {
    match key {
        None => Ok(state.secrets.get(AI_KEY_ACCOUNT)?.filter(|k| !k.is_empty())),
        Some(k) => Ok(Some(k.trim().to_string()).filter(|k| !k.is_empty())),
    }
}

/// The one construction of the HTTP client: the API root and provider of `settings`, with the
/// key one call sends. Every AI entry (model list, settings test, chat) builds its client here.
fn client_of(settings: &AiSettings, key: Option<String>) -> Result<AiClient, AppError> {
    AiClient::new(settings.provider, &settings.base_url, key)
}

pub fn get(state: &AppState) -> Result<AiSettingsView, AppError> {
    Ok(AiSettingsView {
        settings: load(state)?,
        has_key: key_for(state, None)?.is_some(),
    })
}

pub fn save(state: &AppState, settings: AiSettings) -> Result<AiSettingsView, AppError> {
    state.store.save_section(SECTION, &settings.normalized()?)?;
    get(state)
}

/// `None` or an empty key deletes the stored key.
pub fn set_key(state: &AppState, key: Option<String>) -> Result<(), AppError> {
    match key.as_deref().map(str::trim) {
        None | Some("") => state.secrets.delete(AI_KEY_ACCOUNT),
        Some(k) => state.secrets.set(AI_KEY_ACCOUNT, k),
    }
}

/// Model ids of `settings` (saved or not), sorted, without duplicates. The model field is not
/// checked. `key`: as in [`test`].
pub async fn models(
    state: &AppState,
    settings: AiSettings,
    key: Option<String>,
) -> Result<Vec<String>, AppError> {
    let settings = settings.normalized()?;
    let client = client_of(&settings, key_for(state, key)?)?;
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
    require_model(&settings.model)?;
    let client = client_of(&settings, key_for(state, key)?)?;
    let started = Instant::now();
    let models = client.list_models().await?;
    let latency_ms = started.elapsed().as_millis() as u64;
    check_model(settings.provider, &settings.model, &models)?;
    Ok(AiTestResult {
        latency_ms,
        model_count: models.len(),
    })
}

/// The settings and client of one chat: the saved settings, the one key rule, and a model the
/// server lists, so a chat cannot start on a model that only exists in the settings file.
pub async fn for_chat(state: &AppState) -> Result<(AiSettings, AiClient), AppError> {
    let settings = load(state)?.normalized()?;
    require_model(&settings.model)?;
    let client = client_of(&settings, key_for(state, None)?)?;
    check_model(
        settings.provider,
        &settings.model,
        &client.list_models().await?,
    )?;
    Ok((settings, client))
}

fn require_model(model: &str) -> Result<(), AppError> {
    if model.is_empty() {
        Err(AppError::invalid("Choose an AI model in Settings first"))
    } else {
        Ok(())
    }
}

/// `model_not_found` unless `model` is one of `models`. A provider may list a name with a
/// suffix it adds itself (see `Provider::model_suffixes`); either name is accepted.
pub fn check_model(provider: Provider, model: &str, models: &[String]) -> Result<(), AppError> {
    if models.iter().any(|m| {
        m == model
            || provider
                .model_suffixes()
                .iter()
                .any(|suffix| m.strip_suffix(suffix) == Some(model))
    }) {
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
        assert_eq!(s.provider, Provider::OpenAi);
    }

    /// Older files hold provider labels that are not enum values; every label but `anthropic`
    /// is an OpenAI-compatible server, and saving writes the two labels back.
    #[test]
    fn provider_labels_collapse_to_the_wire_format() {
        for (label, provider) in [
            ("ollama", Provider::OpenAi),
            ("custom", Provider::OpenAi),
            ("anthropic", Provider::Anthropic),
            (" openai ", Provider::OpenAi),
        ] {
            let s: AiSettings =
                serde_json::from_str(&format!(r#"{{"provider": "{label}"}}"#)).unwrap();
            assert_eq!(s.provider, provider, "{label}");
        }
        for (provider, label) in [
            (Provider::OpenAi, "openai"),
            (Provider::Anthropic, "anthropic"),
        ] {
            let s = AiSettings {
                provider,
                ..Default::default()
            };
            let v = serde_json::to_value(s).unwrap();
            assert_eq!(v["provider"], label);
        }
    }

    #[test]
    fn read_limits_come_from_the_saved_numbers() {
        let s = AiSettings {
            max_rows: 42,
            timeout_sec: 7,
            ..Default::default()
        };
        let limits = s.read_limits();
        assert_eq!(limits.max_rows, 42);
        assert_eq!(limits.timeout, std::time::Duration::from_secs(7));
        // One default: the settings default and the read path share their numbers.
        let d = AiSettings::default().read_limits();
        assert_eq!(d, ReadLimits::default());
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
        let openai = Provider::OpenAi;
        let models: Vec<String> = (0..25).map(|i| format!("m{i}")).collect();
        assert!(check_model(openai, "m3", &models).is_ok());
        let e = check_model(openai, "nope", &models).unwrap_err();
        assert_eq!(e.code, ErrorCode::ModelNotFound);
        assert_eq!(
            e.message,
            "Model \"nope\" is not on this server. The server lists 25 models."
        );
        let detail = e.detail.unwrap();
        assert_eq!(detail.split(", ").count(), 20);
        assert!(detail.starts_with("m0, m1"));
        let e = check_model(openai, "x", &["only".into()]).unwrap_err();
        assert_eq!(
            e.message,
            "Model \"x\" is not on this server. The server lists 1 model."
        );
        let ollama = ["llama3:latest".to_string(), "qwen2.5:7b".to_string()];
        assert!(check_model(openai, "llama3", &ollama).is_ok());
        assert!(check_model(openai, "llama3:latest", &ollama).is_ok());
        assert!(check_model(openai, "qwen2.5:7b", &ollama).is_ok());
        assert!(check_model(openai, "qwen2.5", &ollama).is_err());
        assert!(check_model(openai, "llama", &ollama).is_err());
        // The implicit tag is the OpenAI-compatible provider's rule; Anthropic lists exact ids.
        assert!(check_model(Provider::Anthropic, "llama3", &ollama).is_err());
        assert!(check_model(Provider::Anthropic, "llama3:latest", &ollama).is_ok());
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
