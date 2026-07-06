# Surface Denylist

This directory contains the short-term runtime surface denylist and the sample
analysis artifacts used to review it.

## Layout

- `reviewed/en.txt` and `reviewed/zh.txt` are the reviewed source lists. They
  contain normalized surface keys, one per line. Empty lines and lines starting
  with `#` are ignored.
- `generated/runtime-zh-en-20260702.surface-ids.json` is a temporary bridge for
  the current runtime data package. It maps reviewed surface text to
  `surface_id` values from that package so the runtime can suppress those
  matches without rebuilding the automaton. Delete this generated bridge when
  denylist filtering moves into the builder pipeline.
- `analysis/samples/out/` contains generated top-surface reports from local
  sample runs.

The sample analysis reports are committed so changes to the analysis script have
a concrete, inspectable output shape. Raw book text is not committed because it
may be copyrighted; generate it under `/tmp` or another private local path.
Full-book analysis output can be generated outside the repo first and copied
here only after review.

Regenerate the bridge with:

```bash
scripts/generate-surface-denylist-ids.py \
  --version zh-en-20260702 \
  --surface-qids data/preprocess/surface_qids.tsv \
  --runtime data/runtime
```

Run the sample analysis with:

```bash
scripts/analyze-surface-frequency.py \
  --raw-dir /tmp/wikispine-denylist-analysis/raw/en \
  --language en \
  --top 20 \
  --out config/surface-denylist/analysis/samples/out/en-top-surfaces.json

scripts/analyze-surface-frequency.py \
  --raw-dir /tmp/wikispine-denylist-analysis/raw/zh-cn \
  --language zh-cn \
  --top 20 \
  --out config/surface-denylist/analysis/samples/out/zh-cn-top-surfaces.json
```
