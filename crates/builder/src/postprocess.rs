use crate::error::{err, Result};
use crate::preprocess::{escape_json, generated_at_unix, path_for_manifest};
use crate::qid::{parse_qid, QID_FLAG_DISAMBIGUATION};
use crate::tsv;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{copy, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Args {
    pub preprocess: PathBuf,
    pub compile: PathBuf,
    pub out: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            preprocess: PathBuf::from("data/preprocess"),
            compile: PathBuf::from("data/compile"),
            out: PathBuf::from("data/runtime"),
        }
    }
}

#[derive(Debug)]
struct SurfaceStats {
    surface_count: usize,
    surface_qid_value_count: usize,
    qid_count: usize,
    flagged_qid_count: usize,
}

#[derive(Debug)]
struct AutomatonStats {
    automaton_bytes: u64,
    states_len: u32,
    mapper_table_len: u32,
    alphabet_size: u32,
    output_count: u32,
    match_kind: u8,
    num_states: u32,
}

#[derive(Debug)]
struct CompileShard {
    shard_id: usize,
    path: PathBuf,
    surface_start: u32,
    surface_count: usize,
    surface_end_exclusive: u32,
    automaton_bytes: u64,
}

#[derive(Debug)]
struct RuntimeShardStats {
    shard_id: usize,
    surface_start: u32,
    surface_count: usize,
    surface_end_exclusive: u32,
    automaton: AutomatonStats,
}

#[derive(Debug)]
struct RuntimeAutomataStats {
    shard_count: usize,
    source_automaton_bytes: u64,
    state_output_count: usize,
    shards: Vec<RuntimeShardStats>,
}

pub fn run(args: Args) -> Result<()> {
    let surface_qids_path = args.preprocess.join("surface_qids.tsv");
    let qid_flags_path = args.preprocess.join("qid_flags.tsv");
    let compile_manifest_path = args.compile.join("manifest.json");
    for path in [&surface_qids_path, &qid_flags_path, &compile_manifest_path] {
        if !path.exists() {
            return Err(err(format!("missing input file: {}", path.display())));
        }
    }

    let tmp_dir = tmp_dir(&args.out);
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    if args.out.exists() {
        fs::remove_dir_all(&args.out)?;
    }
    fs::create_dir_all(tmp_dir.join("automaton"))?;
    fs::create_dir_all(tmp_dir.join("surfaces"))?;
    fs::create_dir_all(tmp_dir.join("qids"))?;

    let (surface_utf16_lengths, surface_stats) = write_surface_tables(
        &surface_qids_path,
        &qid_flags_path,
        &tmp_dir.join("surfaces"),
        &tmp_dir.join("qids"),
    )?;
    let compile_shards = read_compile_shards(&compile_manifest_path, &args.compile)?;
    let automata_stats = write_sharded_automaton_tables(
        &compile_shards,
        &tmp_dir.join("automaton").join("shards"),
        &surface_utf16_lengths,
    )?;
    if automata_stats.state_output_count != surface_stats.surface_count {
        return Err(err(format!(
            "automaton output count {} does not match surface count {}",
            automata_stats.state_output_count, surface_stats.surface_count
        )));
    }

    write_manifest(
        &tmp_dir.join("manifest.json"),
        &args,
        &surface_stats,
        &automata_stats,
    )?;
    fs::rename(&tmp_dir, &args.out)?;
    eprintln!("wrote {}", args.out.display());
    Ok(())
}

fn write_surface_tables(
    surface_qids_path: &Path,
    qid_flags_path: &Path,
    surfaces_out_dir: &Path,
    qids_out_dir: &Path,
) -> Result<(Vec<u32>, SurfaceStats)> {
    let qid_flags = read_qid_flags(qid_flags_path)?;
    let mut surface_utf16_lengths = Vec::<u32>::new();
    let mut qid_set = BTreeMap::<u32, u32>::new();
    let mut surface_qid_value_count = 0usize;

    let mut index = BufWriter::new(File::create(
        surfaces_out_dir.join("surface_qid_index.bin"),
    )?);
    let mut values = BufWriter::new(File::create(
        surfaces_out_dir.join("surface_qid_values.bin"),
    )?);

    for (line_number, line) in BufReader::new(File::open(surface_qids_path)?)
        .lines()
        .enumerate()
    {
        let line = line?;
        if line_number == 0 {
            validate_surface_qids_header(&line)?;
            continue;
        }
        let (surface_key, qids, qid_count) = parse_surface_qids_row(&line, line_number + 1)?;
        if qids.len() != qid_count {
            return Err(err(format!(
                "qid_count mismatch at line {}: parsed {}, declared {}",
                line_number + 1,
                qids.len(),
                qid_count
            )));
        }
        let utf16_len = u32::try_from(surface_key.encode_utf16().count()).map_err(|_| {
            err(format!(
                "surface_key UTF-16 length overflow at line {}",
                line_number + 1
            ))
        })?;
        surface_utf16_lengths.push(utf16_len);

        write_u32(
            &mut index,
            checked_u32(surface_qid_value_count, "surface QID offset"),
        )?;
        write_u32(&mut index, checked_u32(qids.len(), "surface QID length"))?;
        for qid in qids {
            qid_set
                .entry(qid)
                .or_insert_with(|| *qid_flags.get(&qid).unwrap_or(&0));
            write_u32(&mut values, qid)?;
            surface_qid_value_count += 1;
        }
    }
    index.flush()?;
    values.flush()?;

    let mut qid_numbers = BufWriter::new(File::create(qids_out_dir.join("qid_numbers.bin"))?);
    let mut flags = BufWriter::new(File::create(qids_out_dir.join("qid_flags.bin"))?);
    let mut flagged_qid_count = 0usize;
    for (qid, flag) in &qid_set {
        if flag & QID_FLAG_DISAMBIGUATION != 0 {
            flagged_qid_count += 1;
        }
        write_u32(&mut qid_numbers, *qid)?;
        write_u32(&mut flags, *flag)?;
    }
    qid_numbers.flush()?;
    flags.flush()?;

    let surface_count = surface_utf16_lengths.len();
    Ok((
        surface_utf16_lengths,
        SurfaceStats {
            surface_count,
            surface_qid_value_count,
            qid_count: qid_set.len(),
            flagged_qid_count,
        },
    ))
}

fn read_qid_flags(path: &Path) -> Result<BTreeMap<u32, u32>> {
    let mut flags = BTreeMap::new();
    for (line_number, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line_number == 0 {
            if line != "qid\tflags" {
                return Err(err(format!("unexpected qid_flags.tsv header: {line}")));
            }
            continue;
        }
        let mut parts = line.splitn(2, '\t');
        let qid = parts
            .next()
            .ok_or_else(|| err(format!("missing qid at line {}", line_number + 1)))?;
        let flag = parts
            .next()
            .ok_or_else(|| err(format!("missing flags at line {}", line_number + 1)))?;
        let qid = parse_qid(qid, line_number + 1)?;
        let flag = flag.parse::<u32>().map_err(|source| {
            err(format!(
                "invalid flags at line {}: {source}",
                line_number + 1
            ))
        })?;
        flags.insert(qid, flag);
    }
    Ok(flags)
}

fn parse_surface_qids_row(line: &str, line_number: usize) -> Result<(String, Vec<u32>, usize)> {
    let mut parts = line.splitn(3, '\t');
    let surface_key = parts
        .next()
        .ok_or_else(|| err(format!("missing surface_key at line {line_number}")))?;
    let qids = parts
        .next()
        .ok_or_else(|| err(format!("missing qids at line {line_number}")))?;
    let qid_count = parts
        .next()
        .ok_or_else(|| err(format!("missing qid_count at line {line_number}")))?;
    let surface_key = tsv::unescape(surface_key);
    let qids = tsv::unescape(qids)
        .split('|')
        .filter(|value| !value.is_empty())
        .map(|value| parse_qid(value, line_number))
        .collect::<Result<Vec<_>>>()?;
    let qid_count = qid_count
        .parse::<usize>()
        .map_err(|source| err(format!("invalid qid_count at line {line_number}: {source}")))?;
    Ok((surface_key, qids, qid_count))
}

fn read_compile_shards(manifest_path: &Path, compile_dir: &Path) -> Result<Vec<CompileShard>> {
    let manifest = serde_json::from_reader::<_, Value>(File::open(manifest_path)?)?;
    let shards = manifest
        .get("shards")
        .and_then(Value::as_array)
        .ok_or_else(|| err("compile manifest missing shards[]"))?;
    if shards.is_empty() {
        return Err(err("compile manifest has no shards"));
    }

    let mut result = Vec::with_capacity(shards.len());
    for (index, shard) in shards.iter().enumerate() {
        let shard_id = json_usize(shard, "shard_id")?;
        if shard_id != index {
            return Err(err(format!(
                "compile shard id {shard_id} is out of order at index {index}"
            )));
        }
        let relative_path = shard
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| err(format!("compile shard {shard_id} missing path")))?;
        let surface_start = json_u32(shard, "surface_start")?;
        let surface_count = json_usize(shard, "surface_count")?;
        let surface_end_exclusive = json_u32(shard, "surface_end_exclusive")?;
        let expected_end = u64::from(surface_start) + surface_count as u64;
        if expected_end != u64::from(surface_end_exclusive) {
            return Err(err(format!(
                "compile shard {shard_id} range mismatch: start {surface_start} + count {surface_count} != end {surface_end_exclusive}"
            )));
        }
        let automaton_bytes = json_u64(shard, "automaton_bytes")?;
        let path = compile_dir.join(relative_path);
        if !path.exists() {
            return Err(err(format!(
                "compile shard {shard_id} automaton missing: {}",
                path.display()
            )));
        }
        let actual_bytes = path.metadata()?.len();
        if actual_bytes != automaton_bytes {
            return Err(err(format!(
                "compile shard {shard_id} automaton bytes mismatch: manifest {automaton_bytes}, actual {actual_bytes}"
            )));
        }
        result.push(CompileShard {
            shard_id,
            path,
            surface_start,
            surface_count,
            surface_end_exclusive,
            automaton_bytes,
        });
    }
    Ok(result)
}

fn json_usize(object: &Value, field: &str) -> Result<usize> {
    let value = json_u64(object, field)?;
    usize::try_from(value).map_err(|_| err(format!("json field {field} overflows usize: {value}")))
}

fn json_u32(object: &Value, field: &str) -> Result<u32> {
    let value = json_u64(object, field)?;
    u32::try_from(value).map_err(|_| err(format!("json field {field} overflows u32: {value}")))
}

fn json_u64(object: &Value, field: &str) -> Result<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| err(format!("json object missing numeric field {field}")))
}

fn write_sharded_automaton_tables(
    compile_shards: &[CompileShard],
    shards_out_dir: &Path,
    surface_utf16_lengths: &[u32],
) -> Result<RuntimeAutomataStats> {
    let mut shards = Vec::with_capacity(compile_shards.len());
    let mut source_automaton_bytes = 0u64;
    let mut state_output_count = 0usize;

    for shard in compile_shards {
        let shard_out_dir = shards_out_dir.join(format!("{:06}", shard.shard_id));
        fs::create_dir_all(&shard_out_dir)?;
        let automaton = write_automaton_tables(
            &shard.path,
            &shard_out_dir,
            surface_utf16_lengths,
            Some((shard.surface_start, shard.surface_end_exclusive)),
        )?;
        if automaton.output_count as usize != shard.surface_count {
            return Err(err(format!(
                "compile shard {} output count {} does not match manifest surface count {}",
                shard.shard_id, automaton.output_count, shard.surface_count
            )));
        }
        source_automaton_bytes += shard.automaton_bytes;
        state_output_count += automaton.output_count as usize;
        shards.push(RuntimeShardStats {
            shard_id: shard.shard_id,
            surface_start: shard.surface_start,
            surface_count: shard.surface_count,
            surface_end_exclusive: shard.surface_end_exclusive,
            automaton,
        });
    }

    Ok(RuntimeAutomataStats {
        shard_count: shards.len(),
        source_automaton_bytes,
        state_output_count,
        shards,
    })
}

fn write_automaton_tables(
    automaton_path: &Path,
    automaton_out_dir: &Path,
    surface_utf16_lengths: &[u32],
    expected_surface_range: Option<(u32, u32)>,
) -> Result<AutomatonStats> {
    let automaton_bytes = automaton_path.metadata()?.len();
    let mut reader = BufReader::new(File::open(automaton_path)?);

    let states_len = read_u32(&mut reader)?;
    let mut states_out = BufWriter::new(File::create(automaton_out_dir.join("states.bin"))?);
    copy_exact_bytes(
        &mut reader,
        &mut states_out,
        u64::from(states_len) * 16,
        "states",
    )?;
    states_out.flush()?;

    let mapper_table_len = read_u32(&mut reader)?;
    let mut mapper_out = BufWriter::new(File::create(automaton_out_dir.join("char_code_map.bin"))?);
    copy_exact_bytes(
        &mut reader,
        &mut mapper_out,
        u64::from(mapper_table_len) * 4,
        "char_code_map",
    )?;
    mapper_out.flush()?;
    let alphabet_size = read_u32(&mut reader)?;

    let output_count = read_u32(&mut reader)?;
    let mut outputs_out =
        BufWriter::new(File::create(automaton_out_dir.join("state_outputs.bin"))?);
    for _ in 0..output_count {
        let surface_id = read_u32(&mut reader)?;
        let _utf8_len = read_u32(&mut reader)?;
        let parent_output_pos = read_u32(&mut reader)?;
        if let Some((start, end)) = expected_surface_range {
            if surface_id < start || surface_id >= end {
                return Err(err(format!(
                    "automaton output surface_id {surface_id} is outside expected shard range [{start}, {end})"
                )));
            }
        }
        let utf16_len = surface_utf16_lengths
            .get(surface_id as usize)
            .copied()
            .ok_or_else(|| {
                err(format!(
                    "automaton output references unknown surface_id {surface_id}"
                ))
            })?;
        write_u32(&mut outputs_out, surface_id)?;
        write_u32(&mut outputs_out, utf16_len)?;
        write_u32(&mut outputs_out, parent_output_pos)?;
    }
    outputs_out.flush()?;

    let match_kind = read_u8(&mut reader)?;
    let num_states = read_u32(&mut reader)?;
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(err("unexpected trailing bytes in automaton.bin"));
    }

    Ok(AutomatonStats {
        automaton_bytes,
        states_len,
        mapper_table_len,
        alphabet_size,
        output_count,
        match_kind,
        num_states,
    })
}

fn write_manifest(
    path: &Path,
    args: &Args,
    surface_stats: &SurfaceStats,
    automata_stats: &RuntimeAutomataStats,
) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-runtime-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(
        file,
        "  \"preprocess\": \"{}\",",
        escape_json(&path_for_manifest(&args.preprocess))
    )?;
    writeln!(
        file,
        "  \"compile\": \"{}\",",
        escape_json(&path_for_manifest(&args.compile))
    )?;
    writeln!(
        file,
        "  \"out\": \"{}\",",
        escape_json(&path_for_manifest(&args.out))
    )?;
    writeln!(file, "  \"endian\": \"little\",")?;
    writeln!(file, "  \"mode\": \"charwise\",")?;
    writeln!(file, "  \"state_record_bytes\": 16,")?;
    writeln!(file, "  \"state_output_record_bytes\": 12,")?;
    writeln!(file, "  \"surface_qid_index_record_bytes\": 8,")?;
    writeln!(
        file,
        "  \"qid_flag_disambiguation\": {},",
        QID_FLAG_DISAMBIGUATION
    )?;
    writeln!(
        file,
        "  \"surface_count\": {},",
        surface_stats.surface_count
    )?;
    writeln!(
        file,
        "  \"surface_qid_value_count\": {},",
        surface_stats.surface_qid_value_count
    )?;
    writeln!(file, "  \"qid_count\": {},", surface_stats.qid_count)?;
    writeln!(
        file,
        "  \"flagged_qid_count\": {},",
        surface_stats.flagged_qid_count
    )?;
    writeln!(
        file,
        "  \"automaton_shard_count\": {},",
        automata_stats.shard_count
    )?;
    writeln!(
        file,
        "  \"state_output_count\": {},",
        automata_stats.state_output_count
    )?;
    writeln!(
        file,
        "  \"source_automaton_bytes\": {},",
        automata_stats.source_automaton_bytes
    )?;
    writeln!(file, "  \"automaton_shards\": [")?;
    for (index, shard) in automata_stats.shards.iter().enumerate() {
        let comma = if index + 1 == automata_stats.shards.len() {
            ""
        } else {
            ","
        };
        writeln!(file, "    {{")?;
        writeln!(file, "      \"shard_id\": {},", shard.shard_id)?;
        writeln!(file, "      \"surface_start\": {},", shard.surface_start)?;
        writeln!(file, "      \"surface_count\": {},", shard.surface_count)?;
        writeln!(
            file,
            "      \"surface_end_exclusive\": {},",
            shard.surface_end_exclusive
        )?;
        writeln!(
            file,
            "      \"match_kind\": {},",
            shard.automaton.match_kind
        )?;
        writeln!(
            file,
            "      \"states_len\": {},",
            shard.automaton.states_len
        )?;
        writeln!(
            file,
            "      \"num_states\": {},",
            shard.automaton.num_states
        )?;
        writeln!(
            file,
            "      \"mapper_table_len\": {},",
            shard.automaton.mapper_table_len
        )?;
        writeln!(
            file,
            "      \"alphabet_size\": {},",
            shard.automaton.alphabet_size
        )?;
        writeln!(
            file,
            "      \"state_output_count\": {},",
            shard.automaton.output_count
        )?;
        writeln!(
            file,
            "      \"source_automaton_bytes\": {},",
            shard.automaton.automaton_bytes
        )?;
        writeln!(file, "      \"files\": {{")?;
        writeln!(
            file,
            "        \"char_code_map\": \"automaton/shards/{:06}/char_code_map.bin\",",
            shard.shard_id
        )?;
        writeln!(
            file,
            "        \"states\": \"automaton/shards/{:06}/states.bin\",",
            shard.shard_id
        )?;
        writeln!(
            file,
            "        \"state_outputs\": \"automaton/shards/{:06}/state_outputs.bin\"",
            shard.shard_id
        )?;
        writeln!(file, "      }}")?;
        writeln!(file, "    }}{comma}")?;
    }
    writeln!(file, "  ],")?;
    writeln!(file, "  \"files\": {{")?;
    writeln!(file, "    \"automaton_shards\": \"automaton/shards/\",")?;
    writeln!(
        file,
        "    \"surface_qid_index\": \"surfaces/surface_qid_index.bin\","
    )?;
    writeln!(
        file,
        "    \"surface_qid_values\": \"surfaces/surface_qid_values.bin\","
    )?;
    writeln!(file, "    \"qid_numbers\": \"qids/qid_numbers.bin\",")?;
    writeln!(file, "    \"qid_flags\": \"qids/qid_flags.bin\"")?;
    writeln!(file, "  }}")?;
    writeln!(file, "}}")?;
    file.flush()?;
    Ok(())
}

fn validate_surface_qids_header(line: &str) -> Result<()> {
    if line == "surface_key\tqids\tqid_count" {
        Ok(())
    } else {
        Err(err(format!("unexpected surface_qids.tsv header: {line}")))
    }
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u8<R: Read>(reader: &mut R) -> Result<u8> {
    let mut bytes = [0u8; 1];
    reader.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn copy_exact_bytes<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    bytes: u64,
    label: &str,
) -> Result<()> {
    let copied = copy(&mut reader.take(bytes), writer)?;
    if copied == bytes {
        Ok(())
    } else {
        Err(err(format!("unexpected EOF while copying {label}")))
    }
}

fn checked_u32(value: usize, label: &str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("{label} overflowed u32"))
}

fn tmp_dir(out: &Path) -> PathBuf {
    let file_name = out
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    out.with_file_name(format!("{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ac_compile;

    #[test]
    fn postprocess_writes_surface_qid_tables() {
        let root = std::env::temp_dir().join(format!(
            "wikispine-postprocess-test-{}-{}",
            std::process::id(),
            generated_at_unix()
        ));
        let preprocess_dir = root.join("preprocess");
        let compile_dir = root.join("compile");
        let runtime_dir = root.join("runtime");
        fs::create_dir_all(&preprocess_dir).unwrap();
        fs::write(
            preprocess_dir.join("surface_qids.tsv"),
            "surface_key\tqids\tqid_count\n北京\tQ956\t1\n北京大学\tQ13371|Q3918\t2\n大学\tQ3918\t1\n上海\tQ8686\t1\n",
        )
        .unwrap();
        fs::write(
            preprocess_dir.join("qid_flags.tsv"),
            "qid\tflags\nQ956\t0\nQ3918\t0\nQ13371\t1\nQ8686\t0\n",
        )
        .unwrap();
        ac_compile::run(ac_compile::Args {
            preprocess: preprocess_dir.clone(),
            out: compile_dir.clone(),
            limit: None,
            shard_size: 2,
            progress_every: 10,
        })
        .unwrap();

        run(Args {
            preprocess: preprocess_dir,
            compile: compile_dir,
            out: runtime_dir.clone(),
        })
        .unwrap();

        let index = fs::read(runtime_dir.join("surfaces/surface_qid_index.bin")).unwrap();
        assert_eq!(read_u32_at(&index, 0), 0);
        assert_eq!(read_u32_at(&index, 4), 1);
        assert_eq!(read_u32_at(&index, 8), 1);
        assert_eq!(read_u32_at(&index, 12), 2);
        assert_eq!(read_u32_at(&index, 16), 3);
        assert_eq!(read_u32_at(&index, 20), 1);
        assert_eq!(read_u32_at(&index, 24), 4);
        assert_eq!(read_u32_at(&index, 28), 1);

        let values = fs::read(runtime_dir.join("surfaces/surface_qid_values.bin")).unwrap();
        let values = values
            .chunks_exact(4)
            .map(read_u32_chunk)
            .collect::<Vec<_>>();
        assert_eq!(values, vec![956, 13371, 3918, 3918, 8686]);

        let qid_numbers = fs::read(runtime_dir.join("qids/qid_numbers.bin")).unwrap();
        let qid_numbers = qid_numbers
            .chunks_exact(4)
            .map(read_u32_chunk)
            .collect::<Vec<_>>();
        assert_eq!(qid_numbers, vec![956, 3918, 8686, 13371]);

        let qid_flags = fs::read(runtime_dir.join("qids/qid_flags.bin")).unwrap();
        let qid_flags = qid_flags
            .chunks_exact(4)
            .map(read_u32_chunk)
            .collect::<Vec<_>>();
        assert_eq!(qid_flags, vec![0, 0, 0, 1]);

        for shard_id in ["000000", "000001"] {
            assert!(runtime_dir
                .join(format!("automaton/shards/{shard_id}/states.bin"))
                .exists());
            assert!(runtime_dir
                .join(format!("automaton/shards/{shard_id}/char_code_map.bin"))
                .exists());
            assert!(runtime_dir
                .join(format!("automaton/shards/{shard_id}/state_outputs.bin"))
                .exists());
        }
        let manifest = fs::read_to_string(runtime_dir.join("manifest.json")).unwrap();
        assert!(manifest.contains("\"automaton_shard_count\": 2"));
        assert!(manifest.contains("\"surface_count\": 4"));
        assert!(manifest.contains("\"qid_flag_disambiguation\": 1"));

        fs::remove_dir_all(root).unwrap();
    }

    fn read_u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u32_chunk(chunk: &[u8]) -> u32 {
        u32::from_le_bytes(chunk.try_into().unwrap())
    }
}
