//! Test support for the AI modules: a tiny scripted HTTP server and its response parts.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One part of a scripted response: wait `ms`, then write `bytes`.
pub(crate) type Part = (u64, String);

/// The model list every chat validates its settings model against; `state_with` saves `m`.
const MODELS_JSON: &str = r#"{"data":[{"id":"m"}]}"#;

/// A scripted AI server for the chat tests: `GET /models` (the model a chat validates before its
/// first call) is answered here and is not recorded, so the script and the request channel hold
/// the chat calls only. Every other connection gets `script`, then the socket closes.
pub(crate) async fn serve(
    script: Vec<Part>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    serve_with(script, true).await
}

/// A scripted AI server for the model-list and settings tests: every request gets `script`,
/// including `GET /models`, and every request is recorded.
pub(crate) async fn serve_raw(
    script: Vec<Part>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    serve_with(script, false).await
}

async fn serve_with(
    script: Vec<Part>,
    answer_models: bool,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let script = script.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let req = read_request(&mut sock).await;
                if answer_models && is_model_list(&req) {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{MODELS_JSON}",
                        MODELS_JSON.len()
                    );
                    let _ = sock.write_all(reply.as_bytes()).await;
                    let _ = sock.flush().await;
                    let _ = sock.shutdown().await;
                    return;
                }
                let _ = tx.send(req);
                for (ms, bytes) in script {
                    if ms > 0 {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                    if sock.write_all(bytes.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/v1"), rx)
}

fn is_model_list(req: &str) -> bool {
    let line = req.lines().next().unwrap_or_default();
    line.starts_with("GET ") && line.ends_with("/models HTTP/1.1")
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text[..end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub(crate) fn head(status: &str, content_type: &str) -> Part {
    (
        0,
        format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n"),
    )
}

pub(crate) fn sse(v: Value) -> Part {
    (0, format!("data: {v}\n\n"))
}

pub(crate) fn text_chunk(s: &str) -> Part {
    sse(json!({"choices": [{"index": 0, "delta": {"content": s}, "finish_reason": null}]}))
}

pub(crate) fn finish_chunk(reason: &str) -> Part {
    sse(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]}))
}

pub(crate) fn done() -> Part {
    (0, "data: [DONE]\n\n".into())
}
