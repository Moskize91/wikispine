# wikispine

`wikispine` is a Rust project for compiling Wikipedia and Wikidata surface text into local entity candidate indexes.

The project is intended to focus on the upstream dataset-building side:

- ingest Wikipedia/Wikidata dumps
- normalize surface text
- map surfaces to entity identifiers
- compile compact lookup or matching indexes
- emit runtime-friendly dataset artifacts

It is separate from downstream editor integrations, plugins, and agent-facing application code.

## Status

This repository is newly initialized. Implementation details and file formats are still being designed.
