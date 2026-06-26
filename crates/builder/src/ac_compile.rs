use crate::compact_ac::CharwiseDoubleArrayAhoCorasickBuilder;
use crate::error::{err, Result};
use crate::preprocess::{escape_json, generated_at_unix, path_for_manifest};
use crate::tsv;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Args {
    pub preprocess: PathBuf,
    pub out: PathBuf,
    pub limit: Option<usize>,
    pub shard_size: usize,
    pub progress_every: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            preprocess: PathBuf::from("data/preprocess"),
            out: PathBuf::from("data/compile"),
            limit: None,
            shard_size: 250_000,
            progress_every: 100_000,
        }
    }
}

#[derive(Debug, Clone)]
struct PatternEntry {
    surface_id: u32,
    surface_key: String,
}

#[derive(Debug, Clone)]
struct ShardStats {
    shard_id: usize,
    surface_start: u32,
    surface_count: usize,
    pattern_bytes: usize,
    automaton_bytes: u64,
}

pub fn run(args: Args) -> Result<()> {
    if args.progress_every == 0 {
        return Err(err("--progress-every must be greater than zero"));
    }
    if args.shard_size == 0 {
        return Err(err("--shard-size must be greater than zero"));
    }
    let input_path = args.preprocess.join("surface_qids.tsv");
    if !input_path.exists() {
        return Err(err(format!(
            "missing preprocess file: {}",
            input_path.display()
        )));
    }

    let tmp_dir = tmp_dir(&args.out);
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    if args.out.exists() {
        fs::remove_dir_all(&args.out)?;
    }
    fs::create_dir_all(tmp_dir.join("shards"))?;

    let mut shard_patterns = Vec::<PatternEntry>::with_capacity(args.shard_size);
    let mut shard_pattern_bytes = 0usize;
    let mut total_surface_count = 0usize;
    let mut total_pattern_bytes = 0usize;
    let mut total_automaton_bytes = 0u64;
    let mut shard_stats = Vec::<ShardStats>::new();
    for (line_number, line) in BufReader::new(File::open(&input_path)?).lines().enumerate() {
        let line = line?;
        if line_number == 0 {
            validate_surface_qids_header(&line)?;
            continue;
        }
        if let Some(limit) = args.limit {
            if total_surface_count >= limit {
                break;
            }
        }
        let Some((surface_key, _rest)) = line.split_once('\t') else {
            return Err(err(format!(
                "invalid surface_qids row without tab at line {}",
                line_number + 1
            )));
        };
        let surface_key = tsv::unescape(surface_key);
        if surface_key.is_empty() {
            return Err(err(format!(
                "empty surface_key at surface_qids.tsv line {}",
                line_number + 1
            )));
        }
        let surface_id = checked_u32(line_number - 1, "surface_id");
        shard_pattern_bytes += surface_key.len();
        total_pattern_bytes += surface_key.len();
        shard_patterns.push(PatternEntry {
            surface_id,
            surface_key,
        });
        total_surface_count += 1;

        if total_surface_count % args.progress_every == 0 {
            eprintln!(
                "ingested surface_id={} surfaces={} shards={} pattern_bytes={}",
                surface_id,
                total_surface_count,
                shard_stats.len(),
                total_pattern_bytes
            );
        }

        if shard_patterns.len() >= args.shard_size {
            let stats = write_shard(
                &tmp_dir,
                shard_stats.len(),
                std::mem::take(&mut shard_patterns),
                shard_pattern_bytes,
            )?;
            total_automaton_bytes += stats.automaton_bytes;
            shard_stats.push(stats);
            shard_pattern_bytes = 0;
        }
    }

    if total_surface_count == 0 {
        return Err(err("no surface keys found for compile"));
    }

    if !shard_patterns.is_empty() {
        let stats = write_shard(
            &tmp_dir,
            shard_stats.len(),
            std::mem::take(&mut shard_patterns),
            shard_pattern_bytes,
        )?;
        total_automaton_bytes += stats.automaton_bytes;
        shard_stats.push(stats);
    }

    write_manifest(
        &tmp_dir.join("manifest.json"),
        &args,
        &input_path,
        total_surface_count,
        total_pattern_bytes,
        total_automaton_bytes,
        &shard_stats,
    )?;
    fs::rename(&tmp_dir, &args.out)?;
    eprintln!(
        "wrote {} shards={} automaton_bytes={}",
        args.out.display(),
        shard_stats.len(),
        total_automaton_bytes
    );
    Ok(())
}

pub fn build_automaton_bytes(patterns: Vec<String>) -> Result<Vec<u8>> {
    let entries = patterns
        .into_iter()
        .enumerate()
        .map(|(surface_id, pattern)| (pattern, checked_u32(surface_id, "surface_id")));
    build_automaton_bytes_with_values(entries)
}

fn build_automaton_bytes_for_entries(patterns: Vec<PatternEntry>) -> Result<Vec<u8>> {
    let entries = patterns
        .into_iter()
        .map(|entry| (entry.surface_key, entry.surface_id));
    build_automaton_bytes_with_values(entries)
}

fn build_automaton_bytes_with_values<I>(entries: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = (String, u32)>,
{
    let automaton = CharwiseDoubleArrayAhoCorasickBuilder::new()
        .build_with_values(entries)
        .map_err(|source| err(format!("failed to build charwise automaton: {source}")))?;
    Ok(automaton.serialize())
}

fn validate_surface_qids_header(line: &str) -> Result<()> {
    if line == "surface_key\tqids\tqid_count" {
        Ok(())
    } else {
        Err(err(format!("unexpected surface_qids.tsv header: {line}")))
    }
}

fn checked_u32(value: usize, label: &str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("{label} overflowed u32"))
}

fn tmp_dir(out: &Path) -> PathBuf {
    let file_name = out
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("compile");
    out.with_file_name(format!("{file_name}.tmp"))
}

fn write_shard(
    tmp_dir: &Path,
    shard_id: usize,
    patterns: Vec<PatternEntry>,
    pattern_bytes: usize,
) -> Result<ShardStats> {
    let surface_start = patterns
        .first()
        .map(|entry| entry.surface_id)
        .ok_or_else(|| err("cannot compile empty shard"))?;
    let surface_count = patterns.len();
    eprintln!(
        "building shard={shard_id} surface_start={surface_start} surface_count={surface_count} pattern_bytes={pattern_bytes}"
    );
    let automaton_bytes = build_automaton_bytes_for_entries(patterns)?;
    let shard_dir = tmp_dir.join("shards").join(format!("{shard_id:06}"));
    fs::create_dir_all(&shard_dir)?;
    let automaton_path = shard_dir.join("automaton.bin");
    let mut automaton_file = BufWriter::new(File::create(&automaton_path)?);
    automaton_file.write_all(&automaton_bytes)?;
    automaton_file.flush()?;
    let automaton_size = automaton_path.metadata()?.len();

    let stats = ShardStats {
        shard_id,
        surface_start,
        surface_count,
        pattern_bytes,
        automaton_bytes: automaton_size,
    };
    write_shard_manifest(&shard_dir.join("manifest.json"), &stats)?;
    eprintln!(
        "wrote shard={} automaton={} bytes={}",
        shard_id,
        automaton_path.display(),
        automaton_size
    );
    Ok(stats)
}

fn write_shard_manifest(path: &Path, stats: &ShardStats) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-compile-shard-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(file, "  \"shard_id\": {},", stats.shard_id)?;
    writeln!(file, "  \"surface_start\": {},", stats.surface_start)?;
    writeln!(file, "  \"surface_count\": {},", stats.surface_count)?;
    writeln!(
        file,
        "  \"surface_end_exclusive\": {},",
        u64::from(stats.surface_start) + stats.surface_count as u64
    )?;
    writeln!(file, "  \"pattern_bytes\": {},", stats.pattern_bytes)?;
    writeln!(file, "  \"automaton_bytes\": {},", stats.automaton_bytes)?;
    writeln!(file, "  \"files\": [")?;
    writeln!(file, "    \"automaton.bin\",")?;
    writeln!(file, "    \"manifest.json\"")?;
    writeln!(file, "  ]")?;
    writeln!(file, "}}")?;
    file.flush()?;
    Ok(())
}

fn write_manifest(
    path: &Path,
    args: &Args,
    input_path: &Path,
    surface_count: usize,
    pattern_bytes: usize,
    automaton_bytes: u64,
    shards: &[ShardStats],
) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-compile-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(file, "  \"mode\": \"charwise\",")?;
    writeln!(
        file,
        "  \"preprocess\": \"{}\",",
        escape_json(&path_for_manifest(&args.preprocess))
    )?;
    writeln!(
        file,
        "  \"input\": \"{}\",",
        escape_json(&path_for_manifest(input_path))
    )?;
    writeln!(
        file,
        "  \"out\": \"{}\",",
        escape_json(&path_for_manifest(&args.out))
    )?;
    match args.limit {
        Some(limit) => writeln!(file, "  \"limit\": {limit},")?,
        None => writeln!(file, "  \"limit\": null,")?,
    }
    writeln!(file, "  \"shard_size\": {},", args.shard_size)?;
    writeln!(file, "  \"shard_count\": {},", shards.len())?;
    writeln!(file, "  \"surface_count\": {surface_count},")?;
    writeln!(file, "  \"pattern_bytes\": {pattern_bytes},")?;
    writeln!(file, "  \"automaton_bytes\": {automaton_bytes},")?;
    writeln!(file, "  \"shards\": [")?;
    for (index, shard) in shards.iter().enumerate() {
        let comma = if index + 1 == shards.len() { "" } else { "," };
        writeln!(file, "    {{")?;
        writeln!(file, "      \"shard_id\": {},", shard.shard_id)?;
        writeln!(
            file,
            "      \"path\": \"shards/{:06}/automaton.bin\",",
            shard.shard_id
        )?;
        writeln!(file, "      \"surface_start\": {},", shard.surface_start)?;
        writeln!(file, "      \"surface_count\": {},", shard.surface_count)?;
        writeln!(
            file,
            "      \"surface_end_exclusive\": {},",
            u64::from(shard.surface_start) + shard.surface_count as u64
        )?;
        writeln!(file, "      \"pattern_bytes\": {},", shard.pattern_bytes)?;
        writeln!(file, "      \"automaton_bytes\": {}", shard.automaton_bytes)?;
        writeln!(file, "    }}{comma}")?;
    }
    writeln!(file, "  ],")?;
    writeln!(file, "  \"files\": [")?;
    writeln!(file, "    \"manifest.json\",")?;
    writeln!(file, "    \"shards/\"")?;
    writeln!(file, "  ]")?;
    writeln!(file, "}}")?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_ac::CharwiseDoubleArrayAhoCorasick;

    #[test]
    fn automaton_outputs_surface_ids() {
        let bytes = build_automaton_bytes(vec![
            "北京".to_string(),
            "北京大学".to_string(),
            "大学".to_string(),
        ])
        .unwrap();
        let (automaton, rest) = CharwiseDoubleArrayAhoCorasick::<u32>::deserialize(&bytes).unwrap();
        assert!(rest.is_empty());
        let hits = automaton
            .find_overlapping_iter("我在北京大学")
            .map(|m| (m.start(), m.end(), m.value()))
            .collect::<Vec<_>>();
        assert_eq!(hits, vec![(6, 12, 0), (6, 18, 1), (12, 18, 2)]);
    }

    #[test]
    fn shard_automaton_outputs_global_surface_ids() {
        let bytes = build_automaton_bytes_for_entries(vec![
            PatternEntry {
                surface_id: 2,
                surface_key: "大学".to_string(),
            },
            PatternEntry {
                surface_id: 3,
                surface_key: "上海".to_string(),
            },
        ])
        .unwrap();
        let (automaton, rest) = CharwiseDoubleArrayAhoCorasick::<u32>::deserialize(&bytes).unwrap();
        assert!(rest.is_empty());
        let hits = automaton
            .find_overlapping_iter("上海大学")
            .map(|m| (m.start(), m.end(), m.value()))
            .collect::<Vec<_>>();
        assert_eq!(hits, vec![(0, 6, 3), (6, 12, 2)]);
    }

    #[test]
    fn compile_splits_tiny_surface_table_into_shards() {
        let root = std::env::temp_dir().join(format!(
            "wikispine-compile-test-{}-{}",
            std::process::id(),
            generated_at_unix()
        ));
        let preprocess_dir = root.join("preprocess");
        let compile_dir = root.join("compile");
        fs::create_dir_all(&preprocess_dir).unwrap();
        fs::write(
            preprocess_dir.join("surface_qids.tsv"),
            "surface_key\tqids\tqid_count\n北京\tQ956\t1\n北京大学\tQ13371\t1\n大学\tQ3918\t1\n上海\tQ8686\t1\n",
        )
        .unwrap();

        run(Args {
            preprocess: preprocess_dir,
            out: compile_dir.clone(),
            limit: None,
            shard_size: 2,
            progress_every: 10,
        })
        .unwrap();

        assert!(compile_dir.join("manifest.json").exists());
        assert!(compile_dir.join("shards/000000/automaton.bin").exists());
        assert!(compile_dir.join("shards/000001/automaton.bin").exists());
        assert!(!compile_dir.join("shards/000002/automaton.bin").exists());

        let shard0 = fs::read(compile_dir.join("shards/000000/automaton.bin")).unwrap();
        let (automaton0, rest) =
            CharwiseDoubleArrayAhoCorasick::<u32>::deserialize(&shard0).unwrap();
        assert!(rest.is_empty());
        let hits0 = automaton0
            .find_overlapping_iter("我在北京大学")
            .map(|m| m.value())
            .collect::<Vec<_>>();
        assert_eq!(hits0, vec![0, 1]);

        let shard1 = fs::read(compile_dir.join("shards/000001/automaton.bin")).unwrap();
        let (automaton1, rest) =
            CharwiseDoubleArrayAhoCorasick::<u32>::deserialize(&shard1).unwrap();
        assert!(rest.is_empty());
        let hits1 = automaton1
            .find_overlapping_iter("上海大学")
            .map(|m| m.value())
            .collect::<Vec<_>>();
        assert_eq!(hits1, vec![3, 2]);

        fs::remove_dir_all(root).unwrap();
    }
}
