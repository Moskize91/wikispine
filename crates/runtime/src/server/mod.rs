use crate::core::{MatchOptions, MatchSession, MatchStats, RuntimeDataset, ServerEvent};
use crate::{Result, RuntimeError};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const MATCH_HTTP_BODY_LIMIT: usize = 32 * 1024 * 1024;
const MEMORY_RESERVE_ENV: &str = "WIKISPINE_MEMORY_RESERVE";

#[derive(Clone)]
struct AppState {
    runtime: Arc<RuntimeDataset>,
    shutdown: Arc<AtomicBool>,
}

pub async fn serve(dataset: &Path, bind: SocketAddr) -> Result<()> {
    let _memory_reserve = reserve_startup_memory_from_env()?;
    eprintln!("loading dataset {}", dataset.display());
    let runtime = Arc::new(RuntimeDataset::open(dataset)?);
    eprintln!(
        "loaded dataset surfaces={} qids={} shards={}",
        runtime.manifest.surface_count,
        runtime.manifest.qid_count,
        runtime.shard_count()
    );
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = Arc::new(AppState { runtime, shutdown });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metadata", get(metadata))
        .route(
            "/match",
            post(match_http)
                .get(match_ws)
                .layer(DefaultBodyLimit::max(MATCH_HTTP_BODY_LIMIT)),
        )
        .with_state(state.clone());

    let listener = TcpListener::bind(bind).await?;
    eprintln!("listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state.shutdown.clone()))
        .await
        .map_err(|source| RuntimeError::new(source.to_string()))?;
    Ok(())
}

fn reserve_startup_memory_from_env() -> Result<Option<Vec<u8>>> {
    let Some(value) = env::var_os(MEMORY_RESERVE_ENV) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    let bytes = parse_memory_size(&value)?;
    if bytes == 0 {
        return Ok(None);
    }
    eprintln!("reserving startup memory from {MEMORY_RESERVE_ENV}={value} ({bytes} bytes)");
    let mut reserve = vec![0u8; bytes];
    touch_memory_pages(&mut reserve);
    eprintln!("reserved startup memory: {bytes} bytes");
    Ok(Some(reserve))
}

fn touch_memory_pages(bytes: &mut [u8]) {
    const PAGE_SIZE: usize = 4096;
    for index in (0..bytes.len()).step_by(PAGE_SIZE) {
        bytes[index] = bytes[index].wrapping_add(1);
    }
    if let Some(last) = bytes.last_mut() {
        *last = last.wrapping_add(1);
    }
}

fn parse_memory_size(raw: &str) -> Result<usize> {
    let value = raw.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("off") {
        return Ok(0);
    }
    let split_at = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    if split_at == 0 {
        return Err(RuntimeError::new(format!(
            "{MEMORY_RESERVE_ENV} must be a byte count or size like 48G"
        )));
    }
    let number = value[..split_at]
        .parse::<usize>()
        .map_err(|source| RuntimeError::new(format!("invalid {MEMORY_RESERVE_ENV}: {source}")))?;
    let suffix = value[split_at..].trim().to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" | "b" => 1usize,
        "k" | "kb" | "ki" | "kib" => 1024usize,
        "m" | "mb" | "mi" | "mib" => 1024usize.pow(2),
        "g" | "gb" | "gi" | "gib" => 1024usize.pow(3),
        "t" | "tb" | "ti" | "tib" => 1024usize.pow(4),
        _ => {
            return Err(RuntimeError::new(format!(
                "invalid {MEMORY_RESERVE_ENV} suffix: {suffix}"
            )))
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| RuntimeError::new(format!("{MEMORY_RESERVE_ENV} is too large")))
}

async fn shutdown_signal(shutdown: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(signal) = terminate.as_mut() {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    shutdown.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::parse_memory_size;

    #[test]
    fn parses_memory_reserve_sizes() {
        assert_eq!(parse_memory_size("").unwrap(), 0);
        assert_eq!(parse_memory_size("off").unwrap(), 0);
        assert_eq!(parse_memory_size("1024").unwrap(), 1024);
        assert_eq!(parse_memory_size("1K").unwrap(), 1024);
        assert_eq!(parse_memory_size("2M").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_memory_size("3GiB").unwrap(), 3 * 1024 * 1024 * 1024);
    }
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn readyz() -> &'static str {
    "ready\n"
}

async fn metadata(State(state): State<Arc<AppState>>) -> Json<MetadataResponse> {
    let runtime = &state.runtime;
    Json(MetadataResponse {
        format: runtime.manifest.format.clone(),
        surface_normalization: runtime.manifest.surface_normalization.clone(),
        surface_count: runtime.manifest.surface_count,
        qid_count: runtime.manifest.qid_count,
        automaton_shard_count: runtime.manifest.automaton_shard_count,
    })
}

async fn match_http(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MatchRequest>,
) -> Response {
    let options = request.options.unwrap_or_default();
    ndjson_match_response(state, request.text, options)
}

async fn match_ws(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_match_ws(socket, state))
}

async fn handle_match_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let runtime = &state.runtime;
    let mut session = MatchSession::new(runtime.shard_count(), MatchOptions::default());

    while let Some(message) = receiver.next().await {
        if state.shutdown.load(Ordering::SeqCst) {
            let _ = send_json(
                &mut sender,
                &ServerEvent::Interrupted {
                    reason: "shutdown".to_string(),
                },
            )
            .await;
            return;
        }
        let Ok(message) = message else {
            break;
        };
        match message {
            Message::Text(payload) => {
                let request = serde_json::from_str::<WsClientEvent>(&payload);
                let response = match request {
                    Ok(WsClientEvent::Start { options }) => {
                        session =
                            MatchSession::new(runtime.shard_count(), options.unwrap_or_default());
                        Some(WsServerEvent::Started)
                    }
                    Ok(WsClientEvent::Chunk { text: chunk }) => {
                        for event in session.process_chunk(&chunk, runtime) {
                            if send_json(&mut sender, &event).await.is_err() {
                                return;
                            }
                        }
                        Some(WsServerEvent::Ack {
                            received_chars: session.offset_utf16,
                        })
                    }
                    Ok(WsClientEvent::End) => {
                        if send_json(
                            &mut sender,
                            &ServerEvent::Done {
                                stats: MatchStats {
                                    matches: session.match_count,
                                },
                            },
                        )
                        .await
                        .is_err()
                        {
                            return;
                        }
                        session.reset();
                        None
                    }
                    Err(source) => Some(WsServerEvent::Error {
                        message: source.to_string(),
                    }),
                };
                if let Some(response) = response {
                    if send_json(&mut sender, &response).await.is_err() {
                        return;
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(payload) => {
                if sender.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
}

async fn send_json<T: Serialize>(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: &T,
) -> std::result::Result<(), axum::Error> {
    let payload = serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"type":"error","message":"failed to serialize response"}"#.to_string()
    });
    sender.send(Message::Text(payload)).await
}

fn ndjson_match_response(state: Arc<AppState>, text: String, options: MatchOptions) -> Response {
    let (sender, receiver) = mpsc::channel::<std::result::Result<Bytes, RuntimeError>>(32);
    tokio::task::spawn_blocking(move || {
        let mut matches = 0usize;
        if state.shutdown.load(Ordering::SeqCst) {
            let _ = send_ndjson_event(
                &sender,
                ServerEvent::Interrupted {
                    reason: "shutdown".to_string(),
                },
            );
            return;
        }
        state.runtime.for_each_match(&text, &options, |matched| {
            if state.shutdown.load(Ordering::SeqCst) {
                let _ = send_ndjson_event(
                    &sender,
                    ServerEvent::Interrupted {
                        reason: "shutdown".to_string(),
                    },
                );
                return false;
            }
            matches += 1;
            send_ndjson_event(&sender, ServerEvent::Match { r#match: matched })
        });
        if state.shutdown.load(Ordering::SeqCst) {
            let _ = send_ndjson_event(
                &sender,
                ServerEvent::Interrupted {
                    reason: "shutdown".to_string(),
                },
            );
        } else {
            let _ = send_ndjson_event(
                &sender,
                ServerEvent::Done {
                    stats: MatchStats { matches },
                },
            );
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(ReceiverStream::new(receiver)))
        .unwrap()
}

fn send_ndjson_event(
    sender: &mpsc::Sender<std::result::Result<Bytes, RuntimeError>>,
    event: ServerEvent,
) -> bool {
    let line = match serde_json::to_string(&event) {
        Ok(line) => line,
        Err(source) => {
            let _ = sender.blocking_send(Err(RuntimeError::new(source.to_string())));
            return false;
        }
    };
    sender
        .blocking_send(Ok(Bytes::from(format!("{line}\n"))))
        .is_ok()
}

#[derive(Debug, Deserialize)]
struct MatchRequest {
    text: String,
    options: Option<MatchOptions>,
}

#[derive(Debug, Serialize)]
struct MetadataResponse {
    format: String,
    surface_normalization: String,
    surface_count: usize,
    qid_count: usize,
    automaton_shard_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsClientEvent {
    #[serde(rename = "start")]
    Start { options: Option<MatchOptions> },
    #[serde(rename = "chunk")]
    Chunk { text: String },
    #[serde(rename = "end")]
    End,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsServerEvent {
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "ack")]
    Ack { received_chars: usize },
    #[serde(rename = "error")]
    Error { message: String },
}
