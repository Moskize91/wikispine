use crate::error::{err, Result};
use crate::qid::{qid_number_from_str, QID_FLAG_DISAMBIGUATION, WIKIDATA_DISAMBIGUATION_QID};
use crate::tsv;
use crate::wiki_sql::{
    for_insert_values, open_bzip2_or_plain_reader, parse_i32, parse_u64,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Args {
    pub dumps: PathBuf,
    pub out: PathBuf,
    pub wikis: Vec<String>,
    pub date: String,
    pub limit: Option<usize>,
    pub progress_every: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            dumps: PathBuf::from("data/dumps"),
            out: PathBuf::from("data/preprocess"),
            wikis: vec!["zhwiki".to_string(), "enwiki".to_string()],
            date: "latest".to_string(),
            limit: None,
            progress_every: 100_000,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Page {
    title: String,
    qid: Option<u32>,
    is_disambiguation: bool,
}

#[derive(Debug, Clone)]
struct SurfaceSource {
    surface_key: String,
    qid: u32,
}

#[derive(Debug, Default)]
struct WikidataSurfaceStats {
    entities: usize,
    surfaces: usize,
    disambiguation_qids: usize,
}

pub fn run(args: Args) -> Result<()> {
    validate_args(&args)?;

    let out_dir = args.out.clone();
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir)?;
    }
    fs::create_dir_all(&out_dir)?;

    let mut surface_qids = BTreeMap::<String, BTreeSet<u32>>::new();
    let mut qid_flags = BTreeMap::<u32, u32>::new();
    let mut summaries = Vec::<String>::new();

    for wiki in &args.wikis {
        let page_path = wikipedia_dump_path(&args.dumps, wiki, &args.date, "page");
        let page_props_path = wikipedia_dump_path(&args.dumps, wiki, &args.date, "page_props");
        let redirect_path = wikipedia_dump_path(&args.dumps, wiki, &args.date, "redirect");

        eprintln!("processing {wiki} page table");
        let mut pages = read_pages(&page_path, args.limit)?;

        eprintln!("processing {wiki} page_props table");
        let page_props_stats = attach_page_props(&page_props_path, &mut pages, args.limit)?;

        eprintln!("processing {wiki} redirect table");
        let redirects = read_redirects(wiki, &redirect_path, &pages, args.limit)?;
        let page_surfaces = build_page_title_surfaces(&pages);
        for source in page_surfaces.iter().chain(redirects.iter()) {
            surface_qids
                .entry(source.surface_key.clone())
                .or_default()
                .insert(source.qid);
        }
        for page in pages.values() {
            if let Some(qid) = page.qid {
                if page.is_disambiguation {
                    *qid_flags.entry(qid).or_default() |= QID_FLAG_DISAMBIGUATION;
                } else {
                    qid_flags.entry(qid).or_default();
                }
            }
        }

        summaries.push(format!(
            "{wiki}\tpages_ns0={}\tpages_with_qid={}\tdisambiguation_pages={}\tpage_surfaces={}\tredirect_surfaces={}",
            pages.len(),
            page_props_stats.qid_count,
            page_props_stats.disambiguation_count,
            page_surfaces.len(),
            redirects.len()
        ));
    }

    eprintln!("processing Wikidata entity surfaces and disambiguation flags");
    let wikidata_stats = read_wikidata_surfaces_and_flags(
        &wikidata_entities_dump_path(&args.dumps, &args.date),
        &mut surface_qids,
        &mut qid_flags,
        args.limit,
        args.progress_every,
    )?;
    summaries.push(format!(
        "wikidata\tentities={}\tsurfaces={}\tdisambiguation_qids={}",
        wikidata_stats.entities, wikidata_stats.surfaces, wikidata_stats.disambiguation_qids
    ));
    summaries.push(format!(
        "global\tsurface_keys={}\tqid_flags={}",
        surface_qids.len(),
        qid_flags.len()
    ));

    write_surface_qids_tsv(&out_dir.join("surface_qids.tsv"), &surface_qids)?;
    write_qid_flags_tsv(&out_dir.join("qid_flags.tsv"), &qid_flags)?;
    write_manifest(&out_dir.join("manifest.json"), &args, &summaries)?;

    for summary in summaries {
        println!("{summary}");
    }

    Ok(())
}

fn validate_args(args: &Args) -> Result<()> {
    if args.wikis.is_empty() {
        return Err(err("at least one wiki must be selected"));
    }
    if args.progress_every == 0 {
        return Err(err("--progress-every must be greater than zero"));
    }
    validate_date(&args.date)
}

fn read_pages(path: &Path, limit: Option<usize>) -> Result<HashMap<u64, Page>> {
    let mut pages = HashMap::new();
    for_insert_values(path, "page", limit, |fields| {
        if fields.len() < 4 {
            return Ok(());
        }
        let page_id = parse_u64(&fields[0])?;
        let namespace = parse_i32(&fields[1])?;
        if namespace != 0 {
            return Ok(());
        }
        pages.insert(
            page_id,
            Page {
                title: fields[2].clone(),
                qid: None,
                is_disambiguation: false,
            },
        );
        Ok(())
    })?;
    Ok(pages)
}

#[derive(Debug, Default)]
struct PagePropsStats {
    qid_count: usize,
    disambiguation_count: usize,
}

fn attach_page_props(
    path: &Path,
    pages: &mut HashMap<u64, Page>,
    limit: Option<usize>,
) -> Result<PagePropsStats> {
    let mut stats = PagePropsStats::default();
    for_insert_values(path, "page_props", limit, |fields| {
        if fields.len() < 2 {
            return Ok(());
        }
        let page_id = parse_u64(&fields[0])?;
        let Some(page) = pages.get_mut(&page_id) else {
            return Ok(());
        };
        match fields[1].as_str() {
            "wikibase_item" if fields.len() >= 3 => {
                if page.qid.is_none() {
                    stats.qid_count += 1;
                }
                page.qid = qid_number_from_str(&fields[2]);
            }
            "disambiguation" => {
                if !page.is_disambiguation {
                    stats.disambiguation_count += 1;
                }
                page.is_disambiguation = true;
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(stats)
}

fn read_redirects(
    _wiki: &str,
    path: &Path,
    pages: &HashMap<u64, Page>,
    limit: Option<usize>,
) -> Result<Vec<SurfaceSource>> {
    let mut title_to_page_id = HashMap::with_capacity(pages.len());
    for (page_id, page) in pages {
        title_to_page_id.insert(page.title.as_str(), *page_id);
    }

    let mut redirects = Vec::new();
    for_insert_values(path, "redirect", limit, |fields| {
        if fields.len() < 3 {
            return Ok(());
        }
        let source_page_id = parse_u64(&fields[0])?;
        let namespace = parse_i32(&fields[1])?;
        if namespace != 0 {
            return Ok(());
        }
        let Some(source_page) = pages.get(&source_page_id) else {
            return Ok(());
        };
        let Some(target_page_id) = title_to_page_id.get(fields[2].as_str()) else {
            return Ok(());
        };
        let Some(target_page) = pages.get(target_page_id) else {
            return Ok(());
        };
        let Some(qid) = target_page.qid else {
            return Ok(());
        };
        if let Some(surface_key) = normalize_surface_key(&source_page.title) {
            redirects.push(SurfaceSource { surface_key, qid });
        }
        Ok(())
    })?;
    Ok(redirects)
}

fn build_page_title_surfaces(pages: &HashMap<u64, Page>) -> Vec<SurfaceSource> {
    let mut surfaces = Vec::new();
    for page in pages.values() {
        let Some(qid) = page.qid else {
            continue;
        };
        let Some(surface_key) = normalize_surface_key(&page.title) else {
            continue;
        };
        surfaces.push(SurfaceSource { surface_key, qid });
    }
    surfaces
}

fn read_wikidata_surfaces_and_flags(
    path: &Path,
    surface_qids: &mut BTreeMap<String, BTreeSet<u32>>,
    qid_flags: &mut BTreeMap<u32, u32>,
    limit: Option<usize>,
    progress_every: usize,
) -> Result<WikidataSurfaceStats> {
    let reader = open_bzip2_or_plain_reader(path)?;
    let mut stats = WikidataSurfaceStats::default();
    let mut disambiguation_qids = HashSet::<u32>::new();

    for line in reader.lines() {
        let line = line?;
        let line = line.trim().trim_end_matches(',');
        if line.is_empty() || line == "[" || line == "]" {
            continue;
        }
        let entity = serde_json::from_str::<Value>(line)?;
        let Some(qid) = entity
            .get("id")
            .and_then(Value::as_str)
            .and_then(qid_number_from_str)
        else {
            continue;
        };
        qid_flags.entry(qid).or_default();

        if entity_is_disambiguation(&entity) {
            *qid_flags.entry(qid).or_default() |= QID_FLAG_DISAMBIGUATION;
            disambiguation_qids.insert(qid);
        }

        let mut entity_surfaces = BTreeSet::<String>::new();
        collect_wikidata_entity_surfaces(&entity, &mut entity_surfaces);
        for surface_key in entity_surfaces {
            surface_qids.entry(surface_key).or_default().insert(qid);
            stats.surfaces += 1;
        }

        stats.entities += 1;
        if stats.entities % progress_every == 0 {
            eprintln!(
                "wikidata scanned={} surfaces={} disambiguation_qids={}",
                stats.entities,
                stats.surfaces,
                disambiguation_qids.len()
            );
        }
        if let Some(limit) = limit {
            if stats.entities >= limit {
                break;
            }
        }
    }

    stats.disambiguation_qids = disambiguation_qids.len();
    Ok(stats)
}

fn collect_wikidata_entity_surfaces(entity: &Value, surfaces: &mut BTreeSet<String>) {
    if let Some(labels) = entity.get("labels").and_then(Value::as_object) {
        for label in labels.values() {
            if let Some(value) = label.get("value").and_then(Value::as_str) {
                insert_surface(value, surfaces);
            }
        }
    }

    if let Some(aliases) = entity.get("aliases").and_then(Value::as_object) {
        for alias_list in aliases.values() {
            let Some(alias_list) = alias_list.as_array() else {
                continue;
            };
            for alias in alias_list {
                if let Some(value) = alias.get("value").and_then(Value::as_str) {
                    insert_surface(value, surfaces);
                }
            }
        }
    }

    if let Some(sitelinks) = entity.get("sitelinks").and_then(Value::as_object) {
        for sitelink in sitelinks.values() {
            if let Some(title) = sitelink.get("title").and_then(Value::as_str) {
                insert_surface(title, surfaces);
            }
        }
    }
}

fn insert_surface(value: &str, surfaces: &mut BTreeSet<String>) {
    if let Some(surface_key) = normalize_surface_key(value) {
        surfaces.insert(surface_key);
    }
}

fn entity_is_disambiguation(entity: &Value) -> bool {
    let Some(claims) = entity.get("claims").and_then(Value::as_object) else {
        return false;
    };
    let Some(p31_claims) = claims.get("P31").and_then(Value::as_array) else {
        return false;
    };
    p31_claims.iter().any(|claim| {
        claim
            .get("mainsnak")
            .and_then(|value| value.get("datavalue"))
            .and_then(|value| value.get("value"))
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .and_then(qid_number_from_str)
            == Some(WIKIDATA_DISAMBIGUATION_QID)
    })
}

pub fn normalize_surface_key(value: &str) -> Option<String> {
    let normalized = value.replace('_', " ").trim().to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn write_surface_qids_tsv(path: &Path, surface_qids: &BTreeMap<String, BTreeSet<u32>>) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "surface_key\tqids\tqid_count")?;
    for (surface_key, qids) in surface_qids {
        let qids = qids.iter().map(|qid| format!("Q{qid}")).collect::<Vec<_>>();
        writeln!(
            file,
            "{}\t{}\t{}",
            tsv::escape(surface_key),
            tsv::escape(&qids.join("|")),
            qids.len()
        )?;
    }
    file.flush()?;
    Ok(())
}

fn write_qid_flags_tsv(path: &Path, qid_flags: &BTreeMap<u32, u32>) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "qid\tflags")?;
    for (qid, flags) in qid_flags {
        writeln!(file, "Q{qid}\t{flags}")?;
    }
    file.flush()?;
    Ok(())
}

fn write_manifest(path: &Path, args: &Args, summaries: &[String]) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-preprocess-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(file, "  \"date\": \"{}\",", escape_json(&args.date))?;
    writeln!(file, "  \"dumps\": \"{}\",", escape_json(&path_for_manifest(&args.dumps)))?;
    writeln!(file, "  \"out\": \"{}\",", escape_json(&path_for_manifest(&args.out)))?;
    match args.limit {
        Some(limit) => writeln!(file, "  \"limit\": {limit},")?,
        None => writeln!(file, "  \"limit\": null,")?,
    }
    writeln!(file, "  \"wikis\": [")?;
    for (index, wiki) in args.wikis.iter().enumerate() {
        let comma = if index + 1 == args.wikis.len() { "" } else { "," };
        writeln!(file, "    \"{}\"{comma}", escape_json(wiki))?;
    }
    writeln!(file, "  ],")?;
    writeln!(file, "  \"files\": [")?;
    writeln!(file, "    \"surface_qids.tsv\",")?;
    writeln!(file, "    \"qid_flags.tsv\",")?;
    writeln!(file, "    \"manifest.json\"")?;
    writeln!(file, "  ],")?;
    writeln!(file, "  \"summaries\": [")?;
    for (index, summary) in summaries.iter().enumerate() {
        let comma = if index + 1 == summaries.len() { "" } else { "," };
        writeln!(file, "    \"{}\"{comma}", escape_json(summary))?;
    }
    writeln!(file, "  ]")?;
    writeln!(file, "}}")?;
    file.flush()?;
    Ok(())
}

pub fn wikipedia_dump_path(dumps: &Path, wiki: &str, date: &str, component: &str) -> PathBuf {
    dumps
        .join(wiki)
        .join(date)
        .join(format!("{wiki}-{date}-{component}.sql.gz"))
}

pub fn wikidata_entities_dump_path(dumps: &Path, date: &str) -> PathBuf {
    let file_name = if date == "latest" {
        "latest-all.json.bz2".to_string()
    } else {
        format!("wikidata-{date}-all.json.bz2")
    };
    dumps.join("wikidatawiki").join(date).join(file_name)
}

pub fn validate_date(date: &str) -> Result<()> {
    if date == "latest" {
        return Ok(());
    }
    if date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(err(format!("date must be latest or YYYYMMDD, got {date}")))
    }
}

pub fn generated_at_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn path_for_manifest(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_surface_keys() {
        assert_eq!(normalize_surface_key("A_B"), Some("A B".to_string()));
        assert_eq!(normalize_surface_key("  A  "), Some("A".to_string()));
        assert_eq!(normalize_surface_key("   "), None);
    }

    #[test]
    fn detects_direct_wikidata_disambiguation_claim() {
        let entity: Value = serde_json::from_str(
            r#"{"claims":{"P31":[{"mainsnak":{"datavalue":{"value":{"id":"Q4167410"}}}}]}}"#,
        )
        .unwrap();
        assert!(entity_is_disambiguation(&entity));
    }

    #[test]
    fn collects_labels_aliases_and_sitelinks() {
        let entity: Value = serde_json::from_str(
            r#"{
              "labels":{"en":{"value":"Alpha"}},
              "aliases":{"en":[{"value":"A_B"}]},
              "sitelinks":{"enwiki":{"title":"Alpha_(letter)"}}
            }"#,
        )
        .unwrap();
        let mut surfaces = BTreeSet::new();
        collect_wikidata_entity_surfaces(&entity, &mut surfaces);
        assert_eq!(
            surfaces.into_iter().collect::<Vec<_>>(),
            vec!["A B", "Alpha", "Alpha (letter)"]
        );
    }
}
