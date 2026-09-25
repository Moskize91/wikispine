use crate::core::{MatchOptions, MatchSession, MatchStats, RuntimeDataset, ServerEvent};
use crate::{Result, RuntimeError};
use axum::body::Body;
use axum::extract::ws::{close_code, CloseCode, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const MATCH_HTTP_BODY_LIMIT: usize = 32 * 1024 * 1024;
const MATCH_WS_WINDOW_UTF16: usize = 64 * 1024;
const MATCH_WS_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MATCH_WS_MAX_FRAME_BYTES: usize = 1024 * 1024;
const MATCH_WS_QUEUE_CAPACITY: usize = 32;

#[derive(Clone)]
struct AppState {
    runtime: Arc<RuntimeDataset>,
    shutdown: Arc<AtomicBool>,
}

pub async fn serve(dataset: &Path, bind: SocketAddr) -> Result<()> {
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

async fn healthz() -> &'static str {
    "ok\n"
}

async fn readyz() -> &'static str {
    "ready\n"
}

async fn metadata(State(state): State<Arc<AppState>>) -> Json<MetadataResponse> {
    let runtime = state.runtime.clone();
    Json(MetadataResponse {
        format: runtime.manifest.format.clone(),
        surface_normalization: runtime.manifest.surface_normalization.clone(),
        surface_count: runtime.manifest.surface_count,
        max_surface_char_len: runtime.manifest.max_surface_char_len,
        max_surface_utf16_len: runtime.manifest.max_surface_utf16_len,
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
    ws.max_message_size(MATCH_WS_MAX_MESSAGE_BYTES)
        .max_frame_size(MATCH_WS_MAX_FRAME_BYTES)
        .on_upgrade(move |socket| handle_match_ws(socket, state))
}

async fn handle_match_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let runtime = &state.runtime;
    let (command_tx, mut worker_rx) = mpsc::unbounded_channel();
    let (worker_tx, mut output_rx) = mpsc::channel(MATCH_WS_QUEUE_CAPACITY);
    let worker_runtime = runtime.clone();

    tokio::spawn(async move {
        let mut session = MatchSession::new(worker_runtime.shard_count(), MatchOptions::default());
        while let Some(command) = worker_rx.recv().await {
            match command {
                WsWorkerCommand::Start { options } => {
                    session = MatchSession::new(worker_runtime.shard_count(), options);
                    if worker_tx.send(WsWorkerEvent::Started).await.is_err() {
                        return;
                    }
                }
                WsWorkerCommand::Chunk { text, consumed } => {
                    let events = tokio::task::block_in_place(|| {
                        session.process_chunk(&text, &worker_runtime)
                    });
                    for event in events {
                        if worker_tx.send(WsWorkerEvent::Event(event)).await.is_err() {
                            return;
                        }
                    }
                    if worker_tx
                        .send(WsWorkerEvent::Ack { consumed })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                WsWorkerCommand::End => {
                    let events = tokio::task::block_in_place(|| session.finish());
                    for event in events {
                        if worker_tx.send(WsWorkerEvent::Event(event)).await.is_err() {
                            return;
                        }
                    }
                    if worker_tx
                        .send(WsWorkerEvent::Done {
                            matches: session.match_count,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = worker_tx.send(WsWorkerEvent::Finished).await;
                    return;
                }
            }
        }
    });

    if send_json(
        &mut sender,
        &WsServerEvent::Ready {
            max_message_bytes: MATCH_WS_MAX_MESSAGE_BYTES,
            window: MATCH_WS_WINDOW_UTF16,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let mut started = false;
    let mut ended = false;
    let mut pending_window = 0usize;
    let mut worker_closed = false;

    while !worker_closed {
        tokio::select! {
            message = receiver.next(), if !ended => {
                let Some(message) = message else { break; };
        if state.shutdown.load(Ordering::SeqCst) {
            let _ = sender.send(Message::Close(None)).await;
            return;
        }
        let Ok(message) = message else {
            break;
        };
        match message {
            Message::Text(payload) => {
                let request = serde_json::from_str::<WsClientEvent>(&payload);
                match request {
                    Ok(WsClientEvent::Start { options }) => {
                        if started || pending_window != 0 {
                            let _ = close_protocol(&mut sender, close_code::PROTOCOL, "start order").await;
                            return;
                        }
                        started = true;
                        if command_tx.send(WsWorkerCommand::Start { options: options.unwrap_or_default() }).is_err() {
                            return;
                        }
                    }
                    Ok(WsClientEvent::Chunk { text: chunk }) => {
                        if !started || ended {
                            let _ = close_protocol(&mut sender, close_code::PROTOCOL, "chunk order").await;
                            return;
                        }
                        if chunk.is_empty() {
                            let _ = close_protocol(&mut sender, close_code::PROTOCOL, "empty chunk").await;
                            return;
                        }
                        let consumed = chunk.encode_utf16().count();
                        if pending_window.saturating_add(consumed) > MATCH_WS_WINDOW_UTF16 {
                            let _ = close_protocol(&mut sender, close_code::POLICY, "window exceeded").await;
                            return;
                        }
                        pending_window += consumed;
                        if command_tx.send(WsWorkerCommand::Chunk { text: chunk, consumed }).is_err() {
                            return;
                        }
                    }
                    Ok(WsClientEvent::End) => {
                        if !started || ended {
                            let _ = close_protocol(&mut sender, close_code::PROTOCOL, "end order").await;
                            return;
                        }
                        ended = true;
                        if command_tx.send(WsWorkerCommand::End).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = close_protocol(&mut sender, close_code::INVALID, "invalid JSON").await;
                        return;
                    }
                }
            }
            Message::Close(frame) => {
                let _ = sender.send(Message::Close(frame)).await;
                return;
            }
            Message::Ping(payload) => {
                if sender.send(Message::Pong(payload)).await.is_err() {
                    return;
                }
            }
            _ => {}
        }
            }
            output = output_rx.recv() => {
                let Some(output) = output else { break; };
                match output {
                    WsWorkerEvent::Started => {
                        if send_json(&mut sender, &WsServerEvent::Started).await.is_err() { return; }
                    }
                    WsWorkerEvent::Event(event) => {
                        if send_json(&mut sender, &event).await.is_err() { return; }
                    }
                    WsWorkerEvent::Ack { consumed } => {
                        pending_window = pending_window.saturating_sub(consumed);
                        if send_json(&mut sender, &WsServerEvent::Ack { consumed, available: MATCH_WS_WINDOW_UTF16 - pending_window }).await.is_err() { return; }
                    }
                    WsWorkerEvent::Done { matches } => {
                        if send_json(&mut sender, &ServerEvent::Done { stats: MatchStats { matches } }).await.is_err() { return; }
                    }
                    WsWorkerEvent::Finished => {
                        let _ = sender.send(Message::Close(Some(CloseFrame { code: close_code::NORMAL, reason: "".into() }))).await;
                        worker_closed = true;
                    }
                }
            }
        }
    }
}

async fn close_protocol(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    code: CloseCode,
    reason: &str,
) -> std::result::Result<(), axum::Error> {
    sender
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_owned().into(),
        })))
        .await
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
        let mut interrupted = false;
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
                interrupted = true;
                return false;
            }
            matches += 1;
            send_ndjson_event(&sender, ServerEvent::Match { r#match: matched })
        });
        if interrupted || state.shutdown.load(Ordering::SeqCst) {
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
    max_surface_char_len: usize,
    max_surface_utf16_len: usize,
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

enum WsWorkerCommand {
    Start { options: MatchOptions },
    Chunk { text: String, consumed: usize },
    End,
}

enum WsWorkerEvent {
    Started,
    Event(ServerEvent),
    Ack { consumed: usize },
    Done { matches: usize },
    Finished,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsServerEvent {
    #[serde(rename = "ready")]
    Ready {
        max_message_bytes: usize,
        window: usize,
    },
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "ack")]
    Ack { consumed: usize, available: usize },
}
