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

The dataset builder uses four stages:

```text
download -> preprocess -> compile -> postprocess
```

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
wikispine download
wikispine preprocess
wikispine compile
wikispine postprocess
```

Run any command with `--help` for options.
