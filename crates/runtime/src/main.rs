use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::File;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const ROOT_STATE_ID: u32 = 0;
const QID_FLAG_DISAMBIGUATION: u32 = 1;
const DEFAULT_DATASET: &str = "data/runtime";
const DEFAULT_BIND: &str = "127.0.0.1:8719";

type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Debug)]
struct RuntimeError(String);

impl RuntimeError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

impl From<std::io::Error> for RuntimeError {
    fn from(source: std::io::Error) -> Self {
        Self(source.to_string())
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(source: serde_json::Error) -> Self {
        Self(source.to_string())
    }
}

#[derive(Debug, Clone)]
struct Args {
    command: Command,
    dataset: PathBuf,
    bind: SocketAddr,
}

#[derive(Debug, Clone, Copy)]
enum Command {
    Serve,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = parse_args(env::args().skip(1).collect())?;
    match args.command {
        Command::Serve => serve(args).await,
    }
}

async fn serve(args: Args) -> Result<()> {
    eprintln!("loading dataset {}", args.dataset.display());
    let runtime = Arc::new(RuntimeDataset::open(&args.dataset)?);
    eprintln!(
        "loaded dataset surfaces={} qids={} shards={}",
        runtime.manifest.surface_count,
        runtime.manifest.qid_count,
        runtime.shards.len()
    );

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metadata", get(metadata))
        .route("/match", post(match_http).get(match_ws))
        .with_state(runtime);

    let listener = TcpListener::bind(args.bind).await?;
    eprintln!("listening on http://{}", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|source| RuntimeError::new(source.to_string()))?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn parse_args(raw_args: Vec<String>) -> Result<Args> {
    let command = Command::Serve;
    let mut dataset = PathBuf::from(DEFAULT_DATASET);
    let mut bind = DEFAULT_BIND.parse::<SocketAddr>().unwrap();

    let mut index = 0;
    if raw_args.first().is_some_and(|arg| arg == "serve") {
        index = 1;
    }
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--dataset" => {
                index += 1;
                dataset = PathBuf::from(require_value(&raw_args, index, "--dataset")?);
            }
            "--bind" => {
                index += 1;
                bind = require_value(&raw_args, index, "--bind")?
                    .parse::<SocketAddr>()
                    .map_err(|source| RuntimeError::new(format!("invalid --bind: {source}")))?;
            }
            "-h" | "--help" | "help" => {
                print_help();
                std::process::exit(0);
            }
            unknown => return Err(RuntimeError::new(format!("unknown option: {unknown}"))),
        }
        index += 1;
    }
    Ok(Args {
        command,
        dataset,
        bind,
    })
}

fn require_value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| RuntimeError::new(format!("{option} requires a value")))
}

fn print_help() {
    println!("wikispine-runtime");
    println!();
    println!("Usage:");
    println!("  wikispine-runtime serve [options]");
    println!();
    println!("Options:");
    println!("  --dataset <dir>      Runtime dataset directory (default: data/runtime)");
    println!("  --bind <addr:port>   HTTP bind address (default: 127.0.0.1:8719)");
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn readyz() -> &'static str {
    "ready\n"
}

async fn metadata(State(runtime): State<Arc<RuntimeDataset>>) -> Json<MetadataResponse> {
    Json(MetadataResponse {
        format: runtime.manifest.format.clone(),
        surface_count: runtime.manifest.surface_count,
        qid_count: runtime.manifest.qid_count,
        automaton_shard_count: runtime.manifest.automaton_shard_count,
    })
}

async fn match_http(
    State(runtime): State<Arc<RuntimeDataset>>,
    Json(request): Json<MatchRequest>,
) -> Response {
    let options = request.options.unwrap_or_default();
    ndjson_match_response(runtime, request.text, options)
}

async fn match_ws(
    State(runtime): State<Arc<RuntimeDataset>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_match_ws(socket, runtime))
}

async fn handle_match_ws(socket: WebSocket, runtime: Arc<RuntimeDataset>) {
    let (mut sender, mut receiver) = socket.split();
    let mut session = MatchSession::new(runtime.shards.len(), MatchOptions::default());

    while let Some(message) = receiver.next().await {
        let Ok(message) = message else {
            break;
        };
        match message {
            Message::Text(payload) => {
                let request = serde_json::from_str::<WsClientEvent>(&payload);
                let response = match request {
                    Ok(WsClientEvent::Start { options }) => {
                        session =
                            MatchSession::new(runtime.shards.len(), options.unwrap_or_default());
                        Some(WsServerEvent::Started)
                    }
                    Ok(WsClientEvent::Chunk { text: chunk }) => {
                        for event in session.process_chunk(&chunk, &runtime) {
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

fn ndjson_match_response(
    runtime: Arc<RuntimeDataset>,
    text: String,
    options: MatchOptions,
) -> Response {
    let (sender, receiver) = mpsc::channel::<std::result::Result<Bytes, RuntimeError>>(32);
    tokio::task::spawn_blocking(move || {
        let mut matches = 0usize;
        runtime.for_each_match(&text, &options, |matched| {
            matches += 1;
            send_ndjson_event(&sender, ServerEvent::Match { r#match: matched })
        });
        let _ = send_ndjson_event(
            &sender,
            ServerEvent::Done {
                stats: MatchStats { matches },
            },
        );
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

#[derive(Debug)]
struct RuntimeDataset {
    manifest: Manifest,
    shards: Vec<AutomatonShard>,
    surface_qid_index: MmapTable,
    surface_qid_values: MmapTable,
    qid_numbers: MmapTable,
    qid_flags: MmapTable,
}

impl RuntimeDataset {
    fn open(root: &Path) -> Result<Self> {
        let manifest_path = root.join("manifest.json");
        let manifest = serde_json::from_reader::<_, Manifest>(File::open(&manifest_path)?)?;
        if manifest.format != "wikispine-runtime-v1" {
            return Err(RuntimeError::new(format!(
                "unsupported runtime format: {}",
                manifest.format
            )));
        }
        if manifest.endian != "little" || manifest.mode != "charwise" {
            return Err(RuntimeError::new("unsupported runtime dataset encoding"));
        }

        let mut shards = Vec::with_capacity(manifest.automaton_shards.len());
        for shard in &manifest.automaton_shards {
            shards.push(AutomatonShard::open(root, shard)?);
        }

        Ok(Self {
            surface_qid_index: MmapTable::open(
                &root.join(&manifest.files.surface_qid_index),
                8,
                manifest.surface_count,
            )?,
            surface_qid_values: MmapTable::open(
                &root.join(&manifest.files.surface_qid_values),
                4,
                manifest.surface_qid_value_count,
            )?,
            qid_numbers: MmapTable::open(
                &root.join(&manifest.files.qid_numbers),
                4,
                manifest.qid_count,
            )?,
            qid_flags: MmapTable::open(
                &root.join(&manifest.files.qid_flags),
                4,
                manifest.qid_count,
            )?,
            manifest,
            shards,
        })
    }

    fn for_each_match<F>(&self, text: &str, options: &MatchOptions, mut on_match: F)
    where
        F: FnMut(TextMatch) -> bool,
    {
        for shard in &self.shards {
            if !shard.for_each_match(text, self, options, &mut on_match) {
                break;
            }
        }
    }

    fn qids_for_surface(&self, surface_id: u32, options: &MatchOptions) -> Vec<QidCandidate> {
        let Some((offset, len)) = self.surface_qid_range(surface_id) else {
            return Vec::new();
        };
        let mut candidates = Vec::with_capacity(len as usize);
        for index in offset..offset + len {
            let Some(qid) = self.surface_qid_values.u32_at(index as usize) else {
                continue;
            };
            let flags = self.flags_for_qid(qid).unwrap_or(0);
            let disambiguation = flags & QID_FLAG_DISAMBIGUATION != 0;
            if !options.include_disambiguation && disambiguation {
                continue;
            }
            candidates.push(QidCandidate {
                qid: format!("Q{qid}"),
                qid_number: qid,
                disambiguation,
            });
            if options
                .max_candidates_per_surface
                .is_some_and(|max| candidates.len() >= max)
            {
                break;
            }
        }
        candidates
    }

    fn surface_qid_range(&self, surface_id: u32) -> Option<(u32, u32)> {
        let index = surface_id as usize * 2;
        Some((
            self.surface_qid_index.u32_at(index)?,
            self.surface_qid_index.u32_at(index + 1)?,
        ))
    }

    fn flags_for_qid(&self, qid: u32) -> Option<u32> {
        let mut low = 0usize;
        let mut high = self.manifest.qid_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let value = self.qid_numbers.u32_at(mid)?;
            match value.cmp(&qid) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Equal => return self.qid_flags.u32_at(mid),
                std::cmp::Ordering::Greater => high = mid,
            }
        }
        None
    }
}

#[derive(Debug)]
struct MatchSession {
    shard_states: Vec<u32>,
    options: MatchOptions,
    offset_utf16: usize,
    match_count: usize,
}

impl MatchSession {
    fn new(shard_count: usize, options: MatchOptions) -> Self {
        Self {
            shard_states: vec![ROOT_STATE_ID; shard_count],
            options,
            offset_utf16: 0,
            match_count: 0,
        }
    }

    fn reset(&mut self) {
        self.shard_states.fill(ROOT_STATE_ID);
        self.offset_utf16 = 0;
        self.match_count = 0;
    }

    fn process_chunk(&mut self, chunk: &str, dataset: &RuntimeDataset) -> Vec<ServerEvent> {
        let mut matches = Vec::new();
        for (shard_index, shard) in dataset.shards.iter().enumerate() {
            let state_id = self
                .shard_states
                .get_mut(shard_index)
                .expect("session shard states match runtime shards");
            shard.find_matches_from_state(
                chunk,
                *state_id,
                self.offset_utf16,
                dataset,
                &self.options,
                &mut matches,
            );
            *state_id = shard.advance_state(chunk, *state_id);
        }
        self.offset_utf16 += chunk.encode_utf16().count();
        matches.sort_by_key(|matched| (matched.start, matched.end, matched.surface_id));
        self.match_count += matches.len();
        matches
            .into_iter()
            .map(|matched| ServerEvent::Match { r#match: matched })
            .collect()
    }
}

#[derive(Debug)]
struct AutomatonShard {
    shard_id: usize,
    states: MmapTable,
    char_code_map: MmapTable,
    state_outputs: MmapTable,
}

impl AutomatonShard {
    fn open(root: &Path, manifest: &AutomatonShardManifest) -> Result<Self> {
        let files = &manifest.files;
        Ok(Self {
            shard_id: manifest.shard_id,
            states: MmapTable::open(&root.join(&files.states), 16, manifest.states_len)?,
            char_code_map: MmapTable::open(
                &root.join(&files.char_code_map),
                4,
                manifest.mapper_table_len,
            )?,
            state_outputs: MmapTable::open(
                &root.join(&files.state_outputs),
                12,
                manifest.state_output_count,
            )?,
        })
    }

    fn for_each_match<F>(
        &self,
        text: &str,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
        on_match: &mut F,
    ) -> bool
    where
        F: FnMut(TextMatch) -> bool,
    {
        let mut state_id = ROOT_STATE_ID;
        for (end, character) in CharEndIterator::new(text) {
            state_id = self.next_state_id(state_id, character);
            if !self.for_each_output_at_state(state_id, end, dataset, options, on_match) {
                return false;
            }
        }
        true
    }

    fn find_matches_from_state(
        &self,
        text: &str,
        mut state_id: u32,
        base_offset: usize,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
        matches: &mut Vec<TextMatch>,
    ) {
        for (end, character) in CharEndIterator::new(text) {
            state_id = self.next_state_id(state_id, character);
            self.push_outputs_at_state(state_id, base_offset + end, dataset, options, matches);
        }
    }

    fn advance_state(&self, text: &str, mut state_id: u32) -> u32 {
        for character in text.chars() {
            state_id = self.next_state_id(state_id, character);
        }
        state_id
    }

    fn push_outputs_at_state(
        &self,
        state_id: u32,
        end: usize,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
        matches: &mut Vec<TextMatch>,
    ) {
        let mut output_pos = self.state(state_id).and_then(|state| state.output_pos);
        while let Some(position) = output_pos {
            let Some(output) = self.output(position) else {
                break;
            };
            output_pos = output.parent;
            self.push_match(end, output, dataset, options, matches);
        }
    }

    fn for_each_output_at_state<F>(
        &self,
        state_id: u32,
        end: usize,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
        on_match: &mut F,
    ) -> bool
    where
        F: FnMut(TextMatch) -> bool,
    {
        let mut output_pos = self.state(state_id).and_then(|state| state.output_pos);
        while let Some(position) = output_pos {
            let Some(output) = self.output(position) else {
                break;
            };
            output_pos = output.parent;
            if let Some(matched) = self.build_match(end, output, dataset, options) {
                if !on_match(matched) {
                    return false;
                }
            }
        }
        true
    }

    fn push_match(
        &self,
        end: usize,
        output: StateOutput,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
        matches: &mut Vec<TextMatch>,
    ) {
        if let Some(matched) = self.build_match(end, output, dataset, options) {
            matches.push(matched);
        }
    }

    fn build_match(
        &self,
        end: usize,
        output: StateOutput,
        dataset: &RuntimeDataset,
        options: &MatchOptions,
    ) -> Option<TextMatch> {
        let length = output.utf16_len as usize;
        if length > end {
            return None;
        }
        let qids = dataset.qids_for_surface(output.surface_id, options);
        if qids.is_empty() {
            return None;
        }
        Some(TextMatch {
            start: end - length,
            end,
            surface_id: output.surface_id,
            shard_id: self.shard_id,
            qids,
        })
    }

    fn next_state_id(&self, mut state_id: u32, character: char) -> u32 {
        let Some(mapped) = self.mapped_code(character) else {
            return ROOT_STATE_ID;
        };
        loop {
            if let Some(child) = self.child_index(state_id, mapped) {
                return child;
            }
            if state_id == ROOT_STATE_ID {
                return ROOT_STATE_ID;
            }
            let Some(state) = self.state(state_id) else {
                return ROOT_STATE_ID;
            };
            state_id = state.fail;
        }
    }

    fn child_index(&self, state_id: u32, mapped: u32) -> Option<u32> {
        let base = self.state(state_id)?.base?;
        let child = base.get() ^ mapped;
        let state = self.state(child)?;
        if state.check == state_id {
            Some(child)
        } else {
            None
        }
    }

    fn mapped_code(&self, character: char) -> Option<u32> {
        let codepoint = character as u32 as usize;
        let mapped = self.char_code_map.u32_at(codepoint)?;
        if mapped == u32::MAX {
            None
        } else {
            Some(mapped)
        }
    }

    fn state(&self, state_id: u32) -> Option<StateRecord> {
        let index = state_id as usize * 4;
        Some(StateRecord {
            base: NonZeroU32::new(self.states.u32_at(index)?),
            check: self.states.u32_at(index + 1)?,
            fail: self.states.u32_at(index + 2)?,
            output_pos: NonZeroU32::new(self.states.u32_at(index + 3)?),
        })
    }

    fn output(&self, output_pos: NonZeroU32) -> Option<StateOutput> {
        let index = (output_pos.get() - 1) as usize * 3;
        Some(StateOutput {
            surface_id: self.state_outputs.u32_at(index)?,
            utf16_len: self.state_outputs.u32_at(index + 1)?,
            parent: NonZeroU32::new(self.state_outputs.u32_at(index + 2)?),
        })
    }
}

#[derive(Debug)]
struct MmapTable {
    mmap: Mmap,
    record_bytes: usize,
    record_count: usize,
}

impl MmapTable {
    fn open(path: &Path, record_bytes: usize, record_count: usize) -> Result<Self> {
        let file = File::open(path)?;
        let actual_len = file.metadata()?.len() as usize;
        let expected_len = record_bytes
            .checked_mul(record_count)
            .ok_or_else(|| RuntimeError::new(format!("table size overflow: {}", path.display())))?;
        if actual_len != expected_len {
            return Err(RuntimeError::new(format!(
                "unexpected table size for {}: expected {}, got {}",
                path.display(),
                expected_len,
                actual_len
            )));
        }
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self {
            mmap,
            record_bytes,
            record_count,
        })
    }

    fn u32_at(&self, index: usize) -> Option<u32> {
        if index >= self.record_count * (self.record_bytes / 4) {
            return None;
        }
        let offset = index.checked_mul(4)?;
        let bytes = self.mmap.get(offset..offset + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
}

#[derive(Debug, Clone, Copy)]
struct StateRecord {
    base: Option<NonZeroU32>,
    check: u32,
    fail: u32,
    output_pos: Option<NonZeroU32>,
}

#[derive(Debug, Clone, Copy)]
struct StateOutput {
    surface_id: u32,
    utf16_len: u32,
    parent: Option<NonZeroU32>,
}

struct CharEndIterator<'a> {
    inner: std::str::Chars<'a>,
    utf16_pos: usize,
}

impl<'a> CharEndIterator<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            inner: text.chars(),
            utf16_pos: 0,
        }
    }
}

impl Iterator for CharEndIterator<'_> {
    type Item = (usize, char);

    fn next(&mut self) -> Option<Self::Item> {
        let character = self.inner.next()?;
        self.utf16_pos += character.len_utf16();
        Some((self.utf16_pos, character))
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    format: String,
    endian: String,
    mode: String,
    surface_count: usize,
    surface_qid_value_count: usize,
    qid_count: usize,
    automaton_shard_count: usize,
    automaton_shards: Vec<AutomatonShardManifest>,
    files: RuntimeFiles,
}

#[derive(Debug, Deserialize)]
struct AutomatonShardManifest {
    shard_id: usize,
    states_len: usize,
    mapper_table_len: usize,
    state_output_count: usize,
    files: AutomatonShardFiles,
}

#[derive(Debug, Deserialize)]
struct AutomatonShardFiles {
    char_code_map: String,
    states: String,
    state_outputs: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeFiles {
    surface_qid_index: String,
    surface_qid_values: String,
    qid_numbers: String,
    qid_flags: String,
}

#[derive(Debug, Deserialize)]
struct MatchRequest {
    text: String,
    options: Option<MatchOptions>,
}

#[derive(Debug, Clone, Deserialize)]
struct MatchOptions {
    #[serde(default = "default_include_disambiguation")]
    include_disambiguation: bool,
    max_candidates_per_surface: Option<usize>,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            include_disambiguation: true,
            max_candidates_per_surface: None,
        }
    }
}

fn default_include_disambiguation() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct MetadataResponse {
    format: String,
    surface_count: usize,
    qid_count: usize,
    automaton_shard_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ServerEvent {
    #[serde(rename = "match")]
    Match { r#match: TextMatch },
    #[serde(rename = "done")]
    Done { stats: MatchStats },
}

#[derive(Debug, Serialize)]
struct MatchStats {
    matches: usize,
}

#[derive(Debug, Serialize)]
struct TextMatch {
    start: usize,
    end: usize,
    surface_id: u32,
    shard_id: usize,
    qids: Vec<QidCandidate>,
}

#[derive(Debug, Serialize)]
struct QidCandidate {
    qid: String,
    qid_number: u32,
    disambiguation: bool,
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
