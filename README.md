# wikispine

`wikispine` is a local Wikipedia/Wikidata entity candidate matcher.

It loads a prebuilt runtime dataset and matches surface text in input documents, returning Wikidata
QID candidates as NDJSON events. The released CLI is the runtime tool:

```text
wikispine init
wikispine status
wikispine doctor
wikispine normalize "Ｗｉｋｉｐｅｄｉａ_Title"
wikispine match --text "北京大学位于北京。"
wikispine match < input.txt > matches.ndjson
wikispine serve --bind 127.0.0.1:8719
```

The repository also contains `wikispine-builder`, an offline maintenance tool used to build
`data/runtime/` from Wikimedia dumps. The builder is not the published user-facing CLI.

## Install CLI

Install the latest prebuilt CLI on macOS or Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Moskize91/wikispine/main/scripts/install.sh | sh
```

Install a specific released version:

```bash
curl -fsSL https://raw.githubusercontent.com/Moskize91/wikispine/main/scripts/install.sh | sh -s -- --version v0.1.0
```

The installer downloads the matching GitHub Release archive for your OS and CPU architecture,
verifies its SHA256 checksum, and installs `wikispine` to `~/.local/bin` by default. Use
`--bin-dir <dir>` to choose another install directory.

Windows users can download `wikispine-<version>-windows-x86_64.zip` from
[GitHub Releases](https://github.com/Moskize91/wikispine/releases), unzip it, and place
`wikispine.exe` on `PATH`.

After installing the CLI, install the runtime data package:

```bash
wikispine init
wikispine doctor
```

## Versioning

CLI releases use Git tags such as `v0.1.0`. The package file names include the same version, and the
binary reports it with:

```bash
wikispine --version
```

These should agree:

```text
GitHub Release tag: v0.1.0
Archive name:       wikispine-0.1.0-macos-aarch64.tar.gz
CLI output:         wikispine 0.1.0
```

If you need a reproducible install, pass `--version vX.Y.Z` to the installer instead of using the
latest release.

## What It Does

`wikispine` turns input text into local entity candidate matches:

```text
input text -> surface match -> surface_id -> QID candidates
```

The runtime data package contains compact Aho-Corasick automata plus surface-to-QID tables. It does
not require raw Wikimedia dumps, preprocess TSV files, or compile intermediates.

Current entity identifiers are Wikidata QID numbers. The only QID metadata currently exposed is the
disambiguation flag.

## Install Runtime Data

Install the default runtime data package:

```bash
wikispine init
```

Install from a custom URL or local archive:

```bash
wikispine init --url https://example.com/wikispine-runtime-data.zip
wikispine init --file /path/to/wikispine-runtime-data.zip
```

All install sources are checked against the built-in runtime data MD5. Use `--data-dir` when you
want to install or read a non-default runtime dataset:

```bash
wikispine init --data-dir /path/to/runtime
wikispine status --data-dir /path/to/runtime
```

## Check The Dataset

`status` opens the runtime dataset and prints the data directory, install metadata, format,
normalization contract, surface count, QID count, and automaton shard count:

```bash
wikispine status
```

`doctor` performs a stricter operational check. It verifies that the manifest exists, the dataset can
be loaded, and optionally that a service bind address is available:

```bash
wikispine doctor
wikispine doctor --bind 127.0.0.1:8719
```

## Normalize Text

Builder and runtime use the same surface normalization contract. Use `normalize` when debugging why
two strings do or do not match:

```bash
wikispine normalize "Ｗｉｋｉｐｅｄｉａ_Title"
# wikipedia title
```

If no text argument is provided, `normalize` reads stdin.

## Match Text

For a quick single input:

```bash
wikispine match --text "北京大学位于北京。"
```

For batch use, read UTF-8 text from stdin and write NDJSON to stdout:

```bash
wikispine match < input.txt > matches.ndjson
```

Each output line is a JSON event:

```json
{"type":"match","match":{"start":0,"end":4,"surface_id":93172679,"shard_id":1,"qids":[{"qid":"Q16952","qid_number":16952,"disambiguation":false}]}}
{"type":"done","stats":{"matches":1}}
```

Match `start` and `end` are UTF-16 offsets in the original input text, matching JavaScript string
indexing. Matching runs on normalized text internally, but offsets always refer to the original
input.

Useful options:

```bash
wikispine match --exclude-disambiguation < input.txt
wikispine match --max-candidates-per-surface 3 < input.txt
wikispine match --data-dir /path/to/runtime < input.txt
```

## Serve HTTP And WebSocket

Start the runtime service:

```bash
wikispine serve --bind 127.0.0.1:8719
```

HTTP:

```http
POST /match
Content-Type: application/json
Accept: application/x-ndjson
```

```json
{
  "text": "北京大学位于北京。",
  "options": {
    "include_disambiguation": true,
    "max_candidates_per_surface": 3
  }
}
```

The HTTP response is streamed NDJSON. `GET /match` upgrades to WebSocket for chunked streaming
input. Health and metadata endpoints are:

```text
GET /healthz
GET /readyz
GET /metadata
```

See [docs/runtime-api.md](docs/runtime-api.md) for request and response details.

For Docker service deployment, see [docs/service-deployment.md](docs/service-deployment.md).

## Surface Normalization

Wikispine normalizes Wikipedia/Wikidata surface text and user input with the same rules:

- Unicode NFKC compatibility normalization
- full Unicode case folding
- whitespace and visible separators collapsed to ASCII spaces
- combining marks and default-ignorable characters removed
- leading and trailing spaces trimmed

See [docs/surface-normalization.md](docs/surface-normalization.md) for the full contract.

## Maintainer Data Build

The offline builder produces the runtime data package:

```text
wikispine-builder download
wikispine-builder preprocess
wikispine-builder compile
wikispine-builder postprocess
```

The pipeline is intentionally explicit because full builds are large, slow, and memory intensive.
Generated data lives under `data/` and should not be committed.

Default generated layout:

```text
data/
  dumps/       # raw Wikimedia downloads
  preprocess/  # surface_key -> QID[] and QID flags
  compile/     # sharded Aho-Corasick automata
  runtime/     # runtime-readable package
```

See [docs/builder-pipeline.md](docs/builder-pipeline.md) for the builder contract.

## Local Development

Build and install the runtime CLI on the current machine:

```bash
scripts/install-local.sh
```

The workspace package version is used by:

```bash
wikispine --version
wikispine -V
wikispine version
```
