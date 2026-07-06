# Surface Denylist

This directory contains the short-term runtime surface denylist and the
publishable analysis artifacts used to review it.

## Layout

- `reviewed/en.txt` and `reviewed/zh.txt` are the reviewed source lists. They
  contain normalized surface keys, one per line. Empty lines and lines starting
  with `#` are ignored.
- `generated/runtime-zh-en-20260702.surface-ids.json` is a temporary bridge for
  the current runtime data package. It maps reviewed surface text to
  `surface_id` values from that package so the runtime can suppress those
  matches without rebuilding the automaton. Delete this generated bridge when
  denylist filtering moves into the builder pipeline.
- `analysis/full/` contains generated top-surface reports for the reviewed
  full-book runs. These reports are expected to be committed after checking that
  they do not contain raw source text.

Raw book text is not committed because it may be copyrighted; keep it under the
ignored repo-local `tmp/` directory or another private local path. Analysis
outputs can be committed after they are checked to contain only aggregate
surface statistics and QID metadata.

Regenerate the bridge with:

```bash
scripts/generate-surface-denylist-ids.py \
  --version zh-en-20260702 \
  --surface-qids data/preprocess/surface_qids.tsv \
  --runtime data/runtime
```

Run the full analysis with repo-local ignored raw text:

```bash
scripts/analyze-surface-frequency.py \
  --raw-dir tmp/raw/en \
  --language en \
  --top 5000 \
  --out config/surface-denylist/analysis/full/en-top-surfaces.json

scripts/analyze-surface-frequency.py \
  --raw-dir tmp/raw/zh-cn \
  --language zh-cn \
  --top 5000 \
  --out config/surface-denylist/analysis/full/zh-cn-top-surfaces.json
```
