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
    pub progress_every: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            preprocess: PathBuf::from("data/preprocess"),
            out: PathBuf::from("data/compile"),
            limit: None,
            progress_every: 100_000,
        }
    }
}

pub fn run(args: Args) -> Result<()> {
    if args.progress_every == 0 {
        return Err(err("--progress-every must be greater than zero"));
    }
    let input_path = args.preprocess.join("surface_qids.tsv");
    if !input_path.exists() {
        return Err(err(format!("missing preprocess file: {}", input_path.display())));
    }

    let tmp_dir = tmp_dir(&args.out);
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    if args.out.exists() {
        fs::remove_dir_all(&args.out)?;
    }
    fs::create_dir_all(&tmp_dir)?;

    let mut patterns = Vec::<String>::new();
    let mut pattern_bytes = 0usize;
    for (line_number, line) in BufReader::new(File::open(&input_path)?).lines().enumerate() {
        let line = line?;
        if line_number == 0 {
            validate_surface_qids_header(&line)?;
            continue;
        }
        if let Some(limit) = args.limit {
            if patterns.len() >= limit {
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
        pattern_bytes += surface_key.len();
        patterns.push(surface_key);

        if patterns.len() % args.progress_every == 0 {
            eprintln!(
                "ingested surface_id={} surfaces={} pattern_bytes={}",
                patterns.len() - 1,
                patterns.len(),
                pattern_bytes
            );
        }
    }

    if patterns.is_empty() {
        return Err(err("no surface keys found for compile"));
    }

    eprintln!(
        "building charwise AC surfaces={} pattern_bytes={}",
        patterns.len(),
        pattern_bytes
    );
    let surface_count = patterns.len();
    let automaton_bytes = build_automaton_bytes(patterns)?;
    let automaton_path = tmp_dir.join("automaton.bin");
    let mut automaton_file = BufWriter::new(File::create(&automaton_path)?);
    automaton_file.write_all(&automaton_bytes)?;
    automaton_file.flush()?;
    let automaton_size = automaton_path.metadata()?.len();

    write_manifest(
        &tmp_dir.join("manifest.json"),
        &args,
        &input_path,
        surface_count,
        pattern_bytes,
        automaton_size,
    )?;
    fs::rename(&tmp_dir, &args.out)?;
    eprintln!(
        "wrote {} ({} bytes)",
        args.out.join("automaton.bin").display(),
        automaton_size
    );
    Ok(())
}

pub fn build_automaton_bytes(patterns: Vec<String>) -> Result<Vec<u8>> {
    let entries = patterns
        .into_iter()
        .enumerate()
        .map(|(surface_id, pattern)| (pattern, checked_u32(surface_id, "surface_id")));
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
    let file_name = out.file_name().and_then(|name| name.to_str()).unwrap_or("compile");
    out.with_file_name(format!("{file_name}.tmp"))
}

fn write_manifest(
    path: &Path,
    args: &Args,
    input_path: &Path,
    surface_count: usize,
    pattern_bytes: usize,
    automaton_size: u64,
) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-compile-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(file, "  \"mode\": \"charwise\",")?;
    writeln!(file, "  \"preprocess\": \"{}\",", escape_json(&path_for_manifest(&args.preprocess)))?;
    writeln!(file, "  \"input\": \"{}\",", escape_json(&path_for_manifest(input_path)))?;
    writeln!(file, "  \"out\": \"{}\",", escape_json(&path_for_manifest(&args.out)))?;
    match args.limit {
        Some(limit) => writeln!(file, "  \"limit\": {limit},")?,
        None => writeln!(file, "  \"limit\": null,")?,
    }
    writeln!(file, "  \"surface_count\": {surface_count},")?;
    writeln!(file, "  \"pattern_bytes\": {pattern_bytes},")?;
    writeln!(file, "  \"automaton_bytes\": {automaton_size},")?;
    writeln!(file, "  \"files\": [")?;
    writeln!(file, "    \"automaton.bin\",")?;
    writeln!(file, "    \"manifest.json\"")?;
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
}
