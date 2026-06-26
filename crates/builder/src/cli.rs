use crate::download::Component;
use crate::error::{err, Result};
use crate::{ac_compile, download, postprocess, preprocess};
use std::path::PathBuf;

pub fn run(raw_args: Vec<String>) -> Result<()> {
    let mut args = raw_args.into_iter();
    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };
    let rest = args.collect::<Vec<_>>();
    match command.as_str() {
        "download" => download::run(parse_download_args(rest)?),
        "preprocess" | "process" => preprocess::run(parse_preprocess_args(rest)?),
        "compile" => ac_compile::run(parse_compile_args(rest)?),
        "postprocess" => postprocess::run(parse_postprocess_args(rest)?),
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        _ => Err(err(format!("unknown command: {command}"))),
    }
}

fn parse_download_args(raw_args: Vec<String>) -> Result<download::Args> {
    let mut args = download::Args::default();
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--out" => {
                index += 1;
                args.out = PathBuf::from(require_value(&raw_args, index, "--out")?);
            }
            "--wikis" => {
                index += 1;
                args.wikis = split_csv(require_value(&raw_args, index, "--wikis")?);
            }
            "--components" => {
                index += 1;
                args.components = split_csv(require_value(&raw_args, index, "--components")?)
                    .into_iter()
                    .map(|value| Component::parse(&value))
                    .collect::<Result<Vec<_>>>()?;
            }
            "--date" => {
                index += 1;
                args.date = require_value(&raw_args, index, "--date")?.to_string();
            }
            "--user-agent" => {
                index += 1;
                args.user_agent = require_value(&raw_args, index, "--user-agent")?.to_string();
            }
            "--dry-run" => args.dry_run = true,
            "--force" => args.force = true,
            "-h" | "--help" => {
                print_download_help();
                std::process::exit(0);
            }
            unknown => return Err(err(format!("unknown download option: {unknown}"))),
        }
        index += 1;
    }
    Ok(args)
}

fn parse_preprocess_args(raw_args: Vec<String>) -> Result<preprocess::Args> {
    let mut args = preprocess::Args::default();
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--dumps" => {
                index += 1;
                args.dumps = PathBuf::from(require_value(&raw_args, index, "--dumps")?);
            }
            "--out" => {
                index += 1;
                args.out = PathBuf::from(require_value(&raw_args, index, "--out")?);
            }
            "--wikis" => {
                index += 1;
                args.wikis = split_csv(require_value(&raw_args, index, "--wikis")?);
            }
            "--date" => {
                index += 1;
                args.date = require_value(&raw_args, index, "--date")?.to_string();
            }
            "--limit" => {
                index += 1;
                args.limit = Some(parse_usize(
                    require_value(&raw_args, index, "--limit")?,
                    "--limit",
                )?);
            }
            "--progress-every" => {
                index += 1;
                args.progress_every = parse_usize(
                    require_value(&raw_args, index, "--progress-every")?,
                    "--progress-every",
                )?;
            }
            "-h" | "--help" => {
                print_preprocess_help();
                std::process::exit(0);
            }
            unknown => return Err(err(format!("unknown preprocess option: {unknown}"))),
        }
        index += 1;
    }
    Ok(args)
}

fn parse_compile_args(raw_args: Vec<String>) -> Result<ac_compile::Args> {
    let mut args = ac_compile::Args::default();
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--preprocess" => {
                index += 1;
                args.preprocess = PathBuf::from(require_value(&raw_args, index, "--preprocess")?);
            }
            "--out" => {
                index += 1;
                args.out = PathBuf::from(require_value(&raw_args, index, "--out")?);
            }
            "--limit" => {
                index += 1;
                args.limit = Some(parse_usize(
                    require_value(&raw_args, index, "--limit")?,
                    "--limit",
                )?);
            }
            "--shard-size" => {
                index += 1;
                args.shard_size = parse_usize(
                    require_value(&raw_args, index, "--shard-size")?,
                    "--shard-size",
                )?;
            }
            "--progress-every" => {
                index += 1;
                args.progress_every = parse_usize(
                    require_value(&raw_args, index, "--progress-every")?,
                    "--progress-every",
                )?;
            }
            "-h" | "--help" => {
                print_compile_help();
                std::process::exit(0);
            }
            unknown => return Err(err(format!("unknown compile option: {unknown}"))),
        }
        index += 1;
    }
    Ok(args)
}

fn parse_postprocess_args(raw_args: Vec<String>) -> Result<postprocess::Args> {
    let mut args = postprocess::Args::default();
    let mut index = 0;
    while index < raw_args.len() {
        match raw_args[index].as_str() {
            "--preprocess" => {
                index += 1;
                args.preprocess = PathBuf::from(require_value(&raw_args, index, "--preprocess")?);
            }
            "--compile" => {
                index += 1;
                args.compile = PathBuf::from(require_value(&raw_args, index, "--compile")?);
            }
            "--out" => {
                index += 1;
                args.out = PathBuf::from(require_value(&raw_args, index, "--out")?);
            }
            "-h" | "--help" => {
                print_postprocess_help();
                std::process::exit(0);
            }
            unknown => return Err(err(format!("unknown postprocess option: {unknown}"))),
        }
        index += 1;
    }
    Ok(args)
}

fn require_value<'a>(args: &'a [String], index: usize, option: &str) -> Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| err(format!("{option} requires a value")))
}

fn parse_usize(value: &str, option: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|source| err(format!("{option} must be a positive integer: {source}")))
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn print_help() {
    println!("wikispine-builder");
    println!();
    println!("Commands:");
    println!("  download     Download upstream Wikimedia dump files");
    println!("  preprocess   Build surface text -> QID[] tables");
    println!("  compile      Compile surface text into an Aho-Corasick automaton");
    println!("  postprocess  Package runtime automaton and surface QID tables");
}

fn print_download_help() {
    println!("Usage: wikispine-builder download [options]");
    println!("  --out <dir>                  Output directory (default: data/dumps)");
    println!("  --wikis <csv>                Wiki DB names (default: zhwiki,enwiki)");
    println!("  --components <csv>           page,redirect,page_props,wikidata_entities");
    println!("  --date <latest|YYYYMMDD>     Dump date (default: latest)");
    println!("  --user-agent <value>         User-Agent for Wikimedia downloads");
    println!("  --dry-run                    Print URLs without downloading");
    println!("  --force                      Redownload existing final files");
}

fn print_preprocess_help() {
    println!("Usage: wikispine-builder preprocess [options]");
    println!("  --dumps <dir>                Downloaded dump directory (default: data/dumps)");
    println!("  --out <dir>                  Output directory (default: data/preprocess)");
    println!("  --wikis <csv>                Wiki DB names (default: zhwiki,enwiki)");
    println!("  --date <latest|YYYYMMDD>     Dump date (default: latest)");
    println!("  --limit <n>                  Debug limit for parsed rows/entities");
    println!("  --progress-every <n>         Progress interval (default: 100000)");
}

fn print_compile_help() {
    println!("Usage: wikispine-builder compile [options]");
    println!("  --preprocess <dir>           Preprocess directory (default: data/preprocess)");
    println!("  --out <dir>                  Output directory (default: data/compile)");
    println!("  --limit <n>                  Debug limit for surface rows");
    println!("  --shard-size <n>             Surface rows per automaton shard (default: 250000)");
    println!("  --progress-every <n>         Progress interval (default: 100000)");
}

fn print_postprocess_help() {
    println!("Usage: wikispine-builder postprocess [options]");
    println!("  --preprocess <dir>           Preprocess directory (default: data/preprocess)");
    println!("  --compile <dir>              Compile directory (default: data/compile)");
    println!("  --out <dir>                  Output directory (default: data/runtime)");
}
