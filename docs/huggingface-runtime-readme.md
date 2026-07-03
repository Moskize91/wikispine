---
license: other
pretty_name: Wikispine Runtime Data
---

# Wikispine Runtime Data

This dataset hosts prebuilt runtime data packages for the `wikispine` CLI.

`wikispine` uses these files to run local Wikipedia/Wikidata surface matching without requiring users to build the runtime data from Wikimedia dumps.

## Packages

| Version | File |
| --- | --- |
| `zh-en-20260702` | `wikigraph-runtime-data-zh-en-20260702.zip` |

The `wikispine` CLI includes the runtime package index used for default installation and checksum verification.

## Install With The CLI

Install `wikispine`, then initialize the runtime data:

```bash
wikispine init
wikispine doctor
```

By default, `wikispine init` downloads the default package recorded in the CLI's built-in package index.

## Manual Download

Runtime package URLs are derived from the package version:

```text
https://huggingface.co/datasets/moskize/wikispine-runtime/resolve/main/wikigraph-runtime-data-<version>.zip
```

For example:

```bash
curl -L -o wikigraph-runtime-data-zh-en-20260702.zip \
  https://huggingface.co/datasets/moskize/wikispine-runtime/resolve/main/wikigraph-runtime-data-zh-en-20260702.zip
```

## Source And Licensing

The runtime data is derived from Wikimedia project dumps and Wikidata data.

Users are responsible for complying with the applicable Wikimedia and Wikidata licenses when redistributing or using derived data. The Wikispine source code is maintained separately at:

<https://github.com/Moskize91/wikispine>
