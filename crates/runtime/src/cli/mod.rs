use crate::core::{MatchOptions, MatchSession, MatchStats, RuntimeDataset, ServerEvent};
use crate::server;
use crate::{Result, RuntimeError};
use md5::{Digest, Md5};
use reqwest::blocking::Client;
use serde::Serialize;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::ZipArchive;

const DEFAULT_RUNTIME_DATA_URL: &str = "https://example.com/wikispine-runtime-data.zip";
const DEFAULT_RUNTIME_DATA_MD5: &str = "00000000000000000000000000000000";
const DEFAULT_BIND: &str = "127.0.0.1:8719";
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(raw_args: Vec<String>) -> Result<()> {
    let Some(command) = raw_args.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };
    match command {
        "init" => init(parse_init_args(&raw_args[1..])?),
        "status" => status(parse_status_args(&raw_args[1..])?),
        "match" => match_stdin(parse_match_args(&raw_args[1..])?),
        "serve" => {
            let args = parse_serve_args(&raw_args[1..])?;
            server::serve(&args.data_dir, args.bind).await
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        "-V" | "--version" | "version" => {
            print_version();
            Ok(())
        }
        unknown => Err(RuntimeError::new(format!("unknown command: {unknown}"))),
    }
}

#[derive(Debug)]
struct InitArgs {
    source: InitSource,
    data_dir: PathBuf,
}

#[derive(Debug)]
enum InitSource {
    Url(String),
    File(PathBuf),
}

#[derive(Debug)]
struct StatusArgs {
    data_dir: PathBuf,
}

#[derive(Debug)]
struct MatchArgs {
    data_dir: PathBuf,
    options: MatchOptions,
}

#[derive(Debug)]
struct ServeArgs {
    data_dir: PathBuf,
    bind: SocketAddr,
}

fn parse_init_args(args: &[String]) -> Result<InitArgs> {
    let mut url = None::<String>;
    let mut file = None::<PathBuf>;
    let mut data_dir = default_data_dir()?;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--url" => {
                index += 1;
                url = Some(require_value(args, index, "--url")?.to_string());
            }
            "--file" => {
                index += 1;
                file = Some(PathBuf::from(require_value(args, index, "--file")?));
            }
            "--data-dir" => {
                index += 1;
                data_dir = PathBuf::from(require_value(args, index, "--data-dir")?);
            }
            "-h" | "--help" => {
                print_init_help();
                std::process::exit(0);
            }
            unknown => return Err(RuntimeError::new(format!("unknown init option: {unknown}"))),
        }
        index += 1;
    }
    if url.is_some() && file.is_some() {
        return Err(RuntimeError::new("--url and --file are mutually exclusive"));
    }
    let source = match (url, file) {
        (_, Some(path)) => InitSource::File(path),
        (Some(url), None) => InitSource::Url(url),
        (None, None) => InitSource::Url(DEFAULT_RUNTIME_DATA_URL.to_string()),
    };
    Ok(InitArgs { source, data_dir })
}

fn parse_status_args(args: &[String]) -> Result<StatusArgs> {
    let mut data_dir = default_data_dir()?;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--data-dir" => {
                index += 1;
                data_dir = PathBuf::from(require_value(args, index, "--data-dir")?);
            }
            "-h" | "--help" => {
                print_status_help();
                std::process::exit(0);
            }
            unknown => {
                return Err(RuntimeError::new(format!(
                    "unknown status option: {unknown}"
                )))
            }
        }
        index += 1;
    }
    Ok(StatusArgs { data_dir })
}

fn parse_match_args(args: &[String]) -> Result<MatchArgs> {
    let mut data_dir = default_data_dir()?;
    let mut options = MatchOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--data-dir" => {
                index += 1;
                data_dir = PathBuf::from(require_value(args, index, "--data-dir")?);
            }
            "--exclude-disambiguation" => {
                options.include_disambiguation = false;
            }
            "--max-candidates-per-surface" => {
                index += 1;
                options.max_candidates_per_surface = Some(
                    require_value(args, index, "--max-candidates-per-surface")?
                        .parse::<usize>()
                        .map_err(|source| {
                            RuntimeError::new(format!(
                                "--max-candidates-per-surface must be an integer: {source}"
                            ))
                        })?,
                );
            }
            "-h" | "--help" => {
                print_match_help();
                std::process::exit(0);
            }
            unknown => {
                return Err(RuntimeError::new(format!(
                    "unknown match option: {unknown}"
                )))
            }
        }
        index += 1;
    }
    Ok(MatchArgs { data_dir, options })
}

fn parse_serve_args(args: &[String]) -> Result<ServeArgs> {
    let mut data_dir = default_data_dir()?;
    let mut bind = DEFAULT_BIND.parse::<SocketAddr>().unwrap();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--data-dir" | "--dataset" => {
                index += 1;
                data_dir = PathBuf::from(require_value(args, index, "--data-dir")?);
            }
            "--bind" => {
                index += 1;
                bind = require_value(args, index, "--bind")?
                    .parse::<SocketAddr>()
                    .map_err(|source| RuntimeError::new(format!("invalid --bind: {source}")))?;
            }
            "-h" | "--help" => {
                print_serve_help();
                std::process::exit(0);
            }
            unknown => {
                return Err(RuntimeError::new(format!(
                    "unknown serve option: {unknown}"
                )))
            }
        }
        index += 1;
    }
    Ok(ServeArgs { data_dir, bind })
}

fn require_value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| RuntimeError::new(format!("{option} requires a value")))
}

fn init(args: InitArgs) -> Result<()> {
    let archive_path = match args.source {
        InitSource::Url(url) => download_archive(&url)?,
        InitSource::File(path) => path,
    };
    let actual_md5 = md5_file(&archive_path)?;
    if actual_md5 != DEFAULT_RUNTIME_DATA_MD5 {
        return Err(RuntimeError::new(format!(
            "runtime data MD5 mismatch: expected {}, got {}",
            DEFAULT_RUNTIME_DATA_MD5, actual_md5
        )));
    }

    let parent = args
        .data_dir
        .parent()
        .ok_or_else(|| RuntimeError::new("data directory has no parent"))?;
    fs::create_dir_all(parent)?;
    let tmp_dir = parent.join(format!(".wikispine-runtime-install-{}", unix_timestamp()));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    fs::create_dir_all(&tmp_dir)?;
    extract_zip(&archive_path, &tmp_dir)?;

    let extracted_runtime = if tmp_dir.join("manifest.json").exists() {
        tmp_dir.clone()
    } else if tmp_dir.join("runtime/manifest.json").exists() {
        tmp_dir.join("runtime")
    } else if tmp_dir.join("data/runtime/manifest.json").exists() {
        tmp_dir.join("data/runtime")
    } else {
        return Err(RuntimeError::new(
            "archive does not contain manifest.json, runtime/, or data/runtime/",
        ));
    };
    RuntimeDataset::open(&extracted_runtime)?;

    let old_dir = parent.join(format!(".wikispine-runtime-old-{}", unix_timestamp()));
    if args.data_dir.exists() {
        fs::rename(&args.data_dir, &old_dir)?;
    }
    if extracted_runtime == tmp_dir {
        fs::rename(&tmp_dir, &args.data_dir)?;
    } else {
        fs::rename(&extracted_runtime, &args.data_dir)?;
        let _ = fs::remove_dir_all(&tmp_dir);
    }
    if old_dir.exists() {
        fs::remove_dir_all(old_dir)?;
    }
    write_install_state(&args.data_dir, &actual_md5)?;
    eprintln!("installed runtime data to {}", args.data_dir.display());
    Ok(())
}

fn download_archive(url: &str) -> Result<PathBuf> {
    let cache_dir = default_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    let path = cache_dir.join("wikispine-runtime-data.zip");
    eprintln!("downloading {url}");
    let mut response = Client::new()
        .get(url)
        .send()
        .map_err(|source| RuntimeError::new(format!("download failed: {source}")))?;
    if !response.status().is_success() {
        return Err(RuntimeError::new(format!(
            "download failed with status {}",
            response.status()
        )));
    }
    let mut out = File::create(&path)?;
    io::copy(&mut response, &mut out)
        .map_err(|source| RuntimeError::new(format!("download write failed: {source}")))?;
    Ok(path)
}

fn md5_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Md5::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_zip(archive_path: &Path, out_dir: &Path) -> Result<()> {
    let file = File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|source| RuntimeError::new(format!("invalid zip archive: {source}")))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|source| RuntimeError::new(format!("invalid zip entry: {source}")))?;
        let Some(enclosed_name) = entry.enclosed_name() else {
            continue;
        };
        let out_path = out_dir.join(enclosed_name);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

fn status(args: StatusArgs) -> Result<()> {
    let runtime = RuntimeDataset::open(&args.data_dir)?;
    println!("Runtime data: installed");
    println!("Path: {}", args.data_dir.display());
    println!("Format: {}", runtime.manifest.format);
    println!("Surfaces: {}", runtime.manifest.surface_count);
    println!("QIDs: {}", runtime.manifest.qid_count);
    println!("Shards: {}", runtime.manifest.automaton_shard_count);
    Ok(())
}

fn match_stdin(args: MatchArgs) -> Result<()> {
    let runtime = RuntimeDataset::open(&args.data_dir)?;
    let mut session = MatchSession::new(runtime.shard_count(), args.options);
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut pending = Vec::<u8>::new();

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);
        let valid_len = valid_utf8_prefix_len(&pending);
        if valid_len == 0 {
            continue;
        }
        let rest = pending.split_off(valid_len);
        let chunk = std::str::from_utf8(&pending)
            .map_err(|source| RuntimeError::new(format!("invalid UTF-8 input: {source}")))?;
        for event in session.process_chunk(chunk, &runtime) {
            write_event(&mut writer, &event)?;
        }
        pending = rest;
    }
    if !pending.is_empty() {
        return Err(RuntimeError::new("stdin ended with incomplete UTF-8 input"));
    }
    write_event(
        &mut writer,
        &ServerEvent::Done {
            stats: MatchStats {
                matches: session.match_count,
            },
        },
    )?;
    writer.flush()?;
    Ok(())
}

fn valid_utf8_prefix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(error) => error.valid_up_to(),
    }
}

fn write_event(writer: &mut impl Write, event: &ServerEvent) -> Result<()> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_install_state(data_dir: &Path, md5: &str) -> Result<()> {
    let state_path = data_dir
        .parent()
        .ok_or_else(|| RuntimeError::new("data directory has no parent"))?
        .join("install.json");
    let state = InstallState {
        data_dir: data_dir.display().to_string(),
        archive_md5: md5.to_string(),
        installed_at_unix: unix_timestamp(),
    };
    let mut file = File::create(state_path)?;
    serde_json::to_writer_pretty(&mut file, &state)?;
    file.write_all(b"\n")?;
    Ok(())
}

#[derive(Serialize)]
struct InstallState {
    data_dir: String,
    archive_md5: String,
    installed_at_unix: u64,
}

fn default_data_dir() -> Result<PathBuf> {
    Ok(platform_data_home()?.join("wikispine").join("runtime"))
}

fn default_cache_dir() -> Result<PathBuf> {
    Ok(platform_cache_home()?.join("wikispine"))
}

fn platform_data_home() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| RuntimeError::new("LOCALAPPDATA is not set"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library").join("Application Support"))
            .ok_or_else(|| RuntimeError::new("HOME is not set"))
    } else if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        Ok(PathBuf::from(data_home))
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local").join("share"))
            .ok_or_else(|| RuntimeError::new("HOME is not set"))
    }
}

fn platform_cache_home() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| RuntimeError::new("LOCALAPPDATA is not set"))
    } else if cfg!(target_os = "macos") {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library").join("Caches"))
            .ok_or_else(|| RuntimeError::new("HOME is not set"))
    } else if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        Ok(PathBuf::from(cache_home))
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".cache"))
            .ok_or_else(|| RuntimeError::new("HOME is not set"))
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn print_help() {
    println!("wikispine");
    println!();
    println!("Commands:");
    println!("  init     Download or install runtime data");
    println!("  status   Show runtime data status");
    println!("  match    Read text from stdin and write NDJSON matches to stdout");
    println!("  serve    Start HTTP/WebSocket runtime service");
    println!("  version  Show CLI version");
    println!();
    println!("Options:");
    println!("  -V, --version  Show CLI version");
}

fn print_version() {
    println!("wikispine {VERSION}");
}

fn print_init_help() {
    println!("Usage: wikispine init [options]");
    println!("  --url <url>        Download runtime data archive from URL");
    println!("  --file <path>      Install runtime data archive from local ZIP");
    println!("  --data-dir <dir>   Install directory");
}

fn print_status_help() {
    println!("Usage: wikispine status [options]");
    println!("  --data-dir <dir>   Runtime data directory override");
}

fn print_match_help() {
    println!("Usage: wikispine match [options] < input.txt > matches.ndjson");
    println!("  --data-dir <dir>                    Runtime data directory override");
    println!("  --exclude-disambiguation            Exclude disambiguation QID candidates");
    println!("  --max-candidates-per-surface <n>    Limit QID candidates per surface");
}

fn print_serve_help() {
    println!("Usage: wikispine serve [options]");
    println!("  --data-dir <dir>   Runtime data directory override");
    println!("  --bind <addr>      HTTP bind address (default: 127.0.0.1:8719)");
}
