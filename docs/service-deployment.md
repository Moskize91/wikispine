# Service Deployment

This document covers the generic Docker service image. Runtime data is not baked into the image.

## Runtime Data Package

Package a built runtime directory:

```bash
scripts/package-runtime-data.sh --version zh-en-20260702 --source /path/to/runtime
```

Package and publish it to the public Hugging Face dataset repo:

```bash
scripts/package-runtime-data.sh \
  --version zh-en-20260702 \
  --source /path/to/runtime \
  --publish \
  --delete-zip
```

The generated artifact name is:

```text
wikigraph-runtime-data-<version>-<YYYYMMDD>.zip
```

The script writes archive metadata to:

```text
config/runtime-data.json
```

That file records the runtime data version, provider, Hugging Face dataset repo, artifact name, ZIP
MD5, archive byte size, and creation time. The CLI derives the default download URL from those
fields and verifies the ZIP MD5 during `wikispine init`.

The default public dataset repo is:

```text
moskize/wikispine-runtime
```

The derived download URL is:

```text
https://huggingface.co/datasets/<repo_id>/resolve/<revision>/<artifact>
```

## Docker Image

Build the lightweight service image:

```bash
scripts/build-service-image.sh --tag wikispine-service:0.1.0 --load
```

The image contains only the `wikispine` binary. Mount runtime data at container runtime:

```bash
docker run --rm \
  -p 9000:9000 \
  -e WIKISPINE_DATA_DIR=/data/runtime \
  -v /path/to/runtime:/data/runtime:ro \
  wikispine-service:0.1.0
```

The service listens on `0.0.0.0:$PORT`; the image default is `PORT=9000`.

For managed container deployments, set `WIKISPINE_MEMORY_RESERVE` to make the process allocate and
touch startup memory before loading the runtime dataset:

```bash
docker run --rm \
  --memory=64g \
  --memory-swap=64g \
  -p 9000:9000 \
  -e WIKISPINE_DATA_DIR=/data/runtime \
  -e WIKISPINE_MEMORY_RESERVE=48G \
  -v /path/to/runtime:/data/runtime:ro \
  wikispine-service:0.1.0
```

This is a capacity check, not a runtime-data preload. It intentionally fails early when the
configured container memory is too small. Supported suffixes are `K/M/G/T` and
`KiB/MiB/GiB/TiB`; unset, `0`, `off`, and `none` disable the reserve.

Health checks:

```bash
curl http://127.0.0.1:9000/healthz
curl http://127.0.0.1:9000/readyz
curl http://127.0.0.1:9000/metadata
```

Match request:

```bash
curl -sS http://127.0.0.1:9000/match \
  -H 'content-type: application/json' \
  -H 'accept: application/x-ndjson' \
  -d '{"text":"北京大学位于北京。","options":{"max_candidates_per_surface":3}}'
```

`POST /match` accepts complete JSON requests up to 32 MiB. Response output is streamed NDJSON and is
not capped by that request body limit.

## Runtime Signals

The service handles `SIGTERM` and Ctrl-C with graceful shutdown. If shutdown is observed while a
match stream is active, the service emits:

```json
{"type":"interrupted","reason":"shutdown"}
```

Container platforms may still terminate a process before an interruption event can be delivered.
