use crate::error::{err, Result};
use crate::preprocess::{escape_json, generated_at_unix, path_for_manifest, validate_date};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_USER_AGENT: &str = "wikispine/0.1 (+https://github.com/Moskize91/wikispine)";
const WIKIMEDIA_DUMPS_BASE: &str = "https://dumps.wikimedia.org";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Component {
    Page,
    Redirect,
    PageProps,
    WikidataEntities,
}

impl Component {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "page" => Ok(Self::Page),
            "redirect" => Ok(Self::Redirect),
            "page_props" | "page-props" => Ok(Self::PageProps),
            "wikidata_entities" | "wikidata-entities" => Ok(Self::WikidataEntities),
            _ => Err(err(format!("unknown component: {value}"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Redirect => "redirect",
            Self::PageProps => "page_props",
            Self::WikidataEntities => "wikidata_entities",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Args {
    pub out: PathBuf,
    pub wikis: Vec<String>,
    pub components: Vec<Component>,
    pub date: String,
    pub dry_run: bool,
    pub force: bool,
    pub user_agent: String,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            out: PathBuf::from("data/dumps"),
            wikis: vec!["zhwiki".to_string(), "enwiki".to_string()],
            components: vec![
                Component::Page,
                Component::Redirect,
                Component::PageProps,
                Component::WikidataEntities,
            ],
            date: "latest".to_string(),
            dry_run: false,
            force: false,
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

#[derive(Debug)]
struct Target {
    component: Component,
    wiki: Option<String>,
    url: String,
    path: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    if args.wikis.is_empty() {
        return Err(err("at least one wiki must be selected"));
    }
    if args.components.is_empty() {
        return Err(err("at least one component must be selected"));
    }
    validate_date(&args.date)?;

    let targets = build_targets(&args)?;
    if args.dry_run {
        for target in &targets {
            println!("{} -> {}", target.url, target.path.display());
        }
        return Ok(());
    }

    require_curl()?;
    fs::create_dir_all(&args.out)?;
    for target in &targets {
        download_target(target, &args.user_agent, args.force)?;
    }
    write_manifest(&args.out.join("manifest.json"), &args.date, &targets)?;
    eprintln!("wrote {}", args.out.join("manifest.json").display());
    Ok(())
}

fn build_targets(args: &Args) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    for component in &args.components {
        match component {
            Component::Page | Component::Redirect | Component::PageProps => {
                for wiki in &args.wikis {
                    if wiki.trim().is_empty() {
                        return Err(err("wiki names must not be empty"));
                    }
                    targets.push(wikipedia_target(&args.out, wiki, &args.date, *component)?);
                }
            }
            Component::WikidataEntities => {
                targets.push(wikidata_entities_target(&args.out, &args.date));
            }
        }
    }
    Ok(targets)
}

fn wikipedia_target(out: &Path, wiki: &str, date: &str, component: Component) -> Result<Target> {
    let dump_name = match component {
        Component::Page => "page",
        Component::Redirect => "redirect",
        Component::PageProps => "page_props",
        Component::WikidataEntities => {
            return Err(err("wikidata_entities is not a Wikipedia SQL component"))
        }
    };
    let file_name = format!("{wiki}-{date}-{dump_name}.sql.gz");
    Ok(Target {
        component,
        wiki: Some(wiki.to_string()),
        url: format!("{WIKIMEDIA_DUMPS_BASE}/{wiki}/{date}/{file_name}"),
        path: out.join(wiki).join(date).join(file_name),
    })
}

fn wikidata_entities_target(out: &Path, date: &str) -> Target {
    let (url, file_name) = if date == "latest" {
        (
            format!("{WIKIMEDIA_DUMPS_BASE}/wikidatawiki/entities/latest-all.json.bz2"),
            "latest-all.json.bz2".to_string(),
        )
    } else {
        (
            format!(
                "{WIKIMEDIA_DUMPS_BASE}/wikidatawiki/entities/{date}/wikidata-{date}-all.json.bz2"
            ),
            format!("wikidata-{date}-all.json.bz2"),
        )
    };
    Target {
        component: Component::WikidataEntities,
        wiki: None,
        url,
        path: out.join("wikidatawiki").join(date).join(file_name),
    }
}

fn download_target(target: &Target, user_agent: &str, force: bool) -> Result<()> {
    if let Some(parent) = target.path.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.path.exists() && !force {
        eprintln!("exists {}, skipping", target.path.display());
        return Ok(());
    }
    if force && target.path.exists() {
        fs::remove_file(&target.path)?;
    }
    let partial_path = partial_path(&target.path);
    let mut curl = Command::new("curl");
    curl.arg("--fail")
        .arg("--location")
        .arg("--retry")
        .arg("3")
        .arg("--retry-delay")
        .arg("2")
        .arg("--user-agent")
        .arg(user_agent)
        .arg("--output")
        .arg(&partial_path);
    if partial_path.exists() {
        curl.arg("--continue-at").arg("-");
    }
    curl.arg(&target.url);

    eprintln!("downloading {}", target.url);
    let status = curl.status()?;
    if !status.success() {
        return Err(err(format!("curl failed for {}", target.url)));
    }
    fs::rename(&partial_path, &target.path)?;
    eprintln!("wrote {}", target.path.display());
    Ok(())
}

fn write_manifest(path: &Path, date: &str, targets: &[Target]) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "{{")?;
    writeln!(file, "  \"format\": \"wikispine-download-v1\",")?;
    writeln!(file, "  \"generated_at_unix\": {},", generated_at_unix())?;
    writeln!(file, "  \"date\": \"{}\",", escape_json(date))?;
    writeln!(file, "  \"files\": [")?;
    for (index, target) in targets.iter().enumerate() {
        let comma = if index + 1 == targets.len() { "" } else { "," };
        let bytes = target.path.metadata().map(|metadata| metadata.len()).ok();
        writeln!(file, "    {{")?;
        writeln!(
            file,
            "      \"component\": \"{}\",",
            target.component.as_str()
        )?;
        match &target.wiki {
            Some(wiki) => writeln!(file, "      \"wiki\": \"{}\",", escape_json(wiki))?,
            None => writeln!(file, "      \"wiki\": null,")?,
        }
        writeln!(file, "      \"url\": \"{}\",", escape_json(&target.url))?;
        writeln!(
            file,
            "      \"path\": \"{}\",",
            escape_json(&path_for_manifest(&target.path))
        )?;
        match bytes {
            Some(bytes) => writeln!(file, "      \"bytes\": {bytes}")?,
            None => writeln!(file, "      \"bytes\": null")?,
        }
        writeln!(file, "    }}{comma}")?;
    }
    writeln!(file, "  ]")?;
    writeln!(file, "}}")?;
    file.flush()?;
    Ok(())
}

fn require_curl() -> Result<()> {
    let status = Command::new("curl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => Err(err("curl is required for downloads")),
    }
}

fn partial_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");
    path.with_file_name(format!("{file_name}.part"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_download_targets_include_wikipedia_and_wikidata_dumps() {
        let args = Args::default();
        let targets = build_targets(&args).unwrap();
        let paths = targets
            .iter()
            .map(|target| target.path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let urls = targets
            .iter()
            .map(|target| target.url.as_str())
            .collect::<Vec<_>>();

        assert_eq!(targets.len(), 7);
        assert!(paths.contains(&"data/dumps/zhwiki/latest/zhwiki-latest-page.sql.gz".to_string()));
        assert!(
            paths.contains(&"data/dumps/zhwiki/latest/zhwiki-latest-redirect.sql.gz".to_string())
        );
        assert!(
            paths.contains(&"data/dumps/zhwiki/latest/zhwiki-latest-page_props.sql.gz".to_string())
        );
        assert!(paths.contains(&"data/dumps/enwiki/latest/enwiki-latest-page.sql.gz".to_string()));
        assert!(
            paths.contains(&"data/dumps/enwiki/latest/enwiki-latest-redirect.sql.gz".to_string())
        );
        assert!(
            paths.contains(&"data/dumps/enwiki/latest/enwiki-latest-page_props.sql.gz".to_string())
        );
        assert!(paths.contains(&"data/dumps/wikidatawiki/latest/latest-all.json.bz2".to_string()));
        assert!(
            urls.contains(&"https://dumps.wikimedia.org/wikidatawiki/entities/latest-all.json.bz2")
        );
    }

    #[test]
    fn dated_wikidata_entities_target_uses_wikidata_file_name() {
        let target = wikidata_entities_target(Path::new("data/raw"), "20260601");
        assert_eq!(
            target.url,
            "https://dumps.wikimedia.org/wikidatawiki/entities/20260601/wikidata-20260601-all.json.bz2"
        );
        assert_eq!(
            target.path,
            PathBuf::from("data/raw/wikidatawiki/20260601/wikidata-20260601-all.json.bz2")
        );
    }
}
