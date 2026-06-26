# wikispine

`wikispine` is a Rust project for compiling Wikipedia and Wikidata surface text into local entity candidate indexes.

The project is intended to focus on the upstream dataset-building side:

- ingest Wikipedia/Wikidata dumps
- normalize surface text
- map surfaces to entity identifiers
- compile compact lookup or matching indexes
- emit runtime-friendly dataset artifacts

It is separate from downstream editor integrations, plugins, and agent-facing application code.

## Pipeline

The project is split into two parts:

- `wikispine-builder` builds runtime datasets from Wikimedia dumps.
- `wikispine-runtime` serves or queries a built runtime dataset.

The builder uses four stages:

```text
download -> preprocess -> compile -> postprocess
```

`download` stores upstream Wikimedia files under `data/dumps/` by default. The default dataset
inputs are:

- `zhwiki` and `enwiki` `page.sql.gz`
- `zhwiki` and `enwiki` `page_props.sql.gz`
- `zhwiki` and `enwiki` `redirect.sql.gz`
- Wikidata entities `latest-all.json.bz2`

The directory is called `dumps` because these are upstream Wikimedia dump files. It is not a runtime
artifact; it is only needed when rebuilding `preprocess` from raw sources.

`preprocess` is the semantic center of the pipeline. It merges all supported surface text sources
into one stable table:

```text
surface_key -> QID[]
```

The current surface sources are:

- Wikipedia page titles
- Wikipedia redirect titles
- Wikidata labels
- Wikidata aliases
- Wikidata sitelink titles

The compiler only reads `surface_key` and builds an Aho-Corasick automaton whose output value is
`surface_id`, defined as the row number in `surface_qids.tsv`.

Compilation is sharded because the full surface table is too large for ordinary local memory. Each
shard contains at most `--shard-size` surface rows and still emits global `surface_id` values.
Runtime query code should run all shard automatons and merge their matches.

The runtime package maps:

```text
input text -> surface_id -> QID[]
```

There is no intermediate EID space. QIDs are stored directly as `u32` QID numbers. The only QID
metadata currently retained is a direct disambiguation flag from Wikidata `P31 = Q4167410` plus the
Wikipedia `page_props` disambiguation marker when available. The project intentionally does not
build a `P31/P279` topology or entity type graph.

## Commands

```text
wikispine-builder download
wikispine-builder preprocess
wikispine-builder compile
wikispine-builder postprocess

wikispine-runtime serve --dataset data/runtime --bind 127.0.0.1:8719
```

Run commands with `--help` for options. Runtime serves `POST /match` for HTTP NDJSON responses and
`GET /match` for WebSocket streaming.

Default generated data layout:

```text
data/
  dumps/       # raw Wikimedia downloads
  preprocess/  # surface_key -> QID[] and QID flags
  compile/     # sharded Aho-Corasick automata
  runtime/     # runtime-readable package
```
