# Surface Denylist

This directory contains the short-term runtime surface denylist.

`en.txt` and `zh.txt` are the reviewed source lists. They contain normalized
surface keys, one per line. Empty lines and lines starting with `#` are ignored.

`runtime-zh-en-20260702.surface-ids.json` is a temporary bridge for the current
runtime data package. It maps reviewed surface text to `surface_id` values from
that package so the runtime can suppress those matches without rebuilding the
automaton. Delete this generated bridge when denylist filtering moves into the
builder pipeline.

Regenerate the bridge with:

```bash
scripts/generate-surface-denylist-ids.py \
  --version zh-en-20260702 \
  --surface-qids data/preprocess/surface_qids.tsv \
  --runtime data/runtime
```
