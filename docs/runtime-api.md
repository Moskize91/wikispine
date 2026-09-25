# Runtime API

Runtime 是一个读取 `data/runtime/` 的服务进程。它提供两种入口：HTTP 完整输入和 WebSocket 双向流。两者输出相同的 match event。

本地 CLI 入口是 `wikispine match`，从 stdin 读取文本流并向 stdout 写出 NDJSON match event。服务入口是 `wikispine serve`。

所有入口都会先按 `docs/surface-normalization.md` 标准化查询文本，再进入自动机。输出的 `start` 和 `end` 仍然是原始输入文本的 UTF-16 offset。

## CLI Install

`wikispine init` 安装 runtime 数据包。默认下载地址由 CLI 内置的 runtime package index 推导，也可以指定内置索引中的历史版本、镜像 URL 或本地 ZIP 文件：

```text
wikispine init
wikispine init --version zh-en-20260702
wikispine init --version zh-en-20260702 --url https://example.com/wikigraph-runtime-data-zh-en-20260702.zip
wikispine init --version zh-en-20260702 --file /path/to/wikigraph-runtime-data-zh-en-20260702.zip
```

默认安装和指定 `--version` 的安装必须通过 CLI 内置的 ZIP MD5 校验；CLI 不提供覆盖 MD5 的参数。自定义 `--url` 或 `--file` 如果没有指定 `--version`，会作为外部来源安装并跳过 MD5 校验。

`--data-dir` 是安装目标目录：CLI 会把 ZIP 解压并替换到这个目录。它不是“从文件夹复制”的来源参数。运行命令默认读取平台数据目录下的 runtime 数据，也允许用 `--data-dir` 静默覆盖：

```text
wikispine init --data-dir /path/to/runtime
wikispine match --data-dir /path/to/runtime
wikispine serve --data-dir /path/to/runtime
```

本地 release 第一版构造这些平台：

- `linux-x86_64`
- `macos-aarch64`
- `macos-x86_64`
- `windows-x86_64`

## HTTP Match

`POST /match` 接收完整 JSON request。服务端在 request body 完整到达后开始识别，并以 NDJSON 流式返回结果。

Request:

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

Response:

```json
{"type":"match","match":{"start":0,"end":4,"surface_id":93172679,"shard_id":1,"qids":[{"qid":"Q16952","qid_number":16952,"disambiguation":false}]}}
{"type":"done","stats":{"matches":1}}
```

注意事项：

- HTTP request 不是双向流；客户端必须先提交完整 `text`。
- HTTP response 是流式 NDJSON；客户端应按行读取。
- `start` 和 `end` 是原始输入文本的 UTF-16 offset，和 JavaScript 字符串索引一致。
- 当前输出顺序按 automaton shard 扫描顺序产生，不承诺全局按 offset 排序。

## WebSocket Match

`GET /match` 可以升级为 WebSocket。每条连接表示一条连续的逻辑文本流；客户端可以在服务端消费前持续提交 chunk，服务端通过有界 window 提供背压。

连接建立后，服务端先发送：

```json
{"type":"ready","max_message_bytes":1048576,"window":65536}
```

`max_message_bytes` 是 WebSocket 协议层对单条 message payload 的硬限制，包含完整 JSON 报文。超限时服务端在 JSON 解析前关闭连接，不发送应用层错误。`window` 使用 JavaScript `String.length` 的 UTF-16 code unit 数量；它限制已接收但尚未被 matcher 消费的文本积压。

Client events:

```json
{"type":"start","options":{"include_disambiguation":true,"max_candidates_per_surface":3}}
{"type":"chunk","text":"北京"}
{"type":"chunk","text":"大学"}
{"type":"end"}
```

Server events:

```json
{"type":"started"}
{"type":"match","match":{"start":0,"end":4,"surface_id":93172679,"shard_id":1,"qids":[{"qid":"Q16952","qid_number":16952,"disambiguation":false}]}}
{"type":"ack","consumed":2,"available":65534}
{"type":"done","stats":{"matches":1}}
```

注意事项：

- WebSocket 连接只承载一条逻辑文本流。`chunk` 只是传输分片，不是独立的 match 请求；服务端按连接维护 automaton 和 normalizer state，因此可以识别跨 chunk 的 surface。
- 客户端可以在 window 尚有额度时持续发送 chunk。每个 chunk 被 matcher 消费后，服务端通过 `ack.consumed` 归还额度。
- `end` 表示客户端不会再提交文本。服务端会消费完队列、推送所有 match、推送 `done`，然后主动以正常 close 关闭连接。
- 超过当前 window、违反消息顺序或发送无效 JSON 时，服务端直接关闭 WebSocket；超大 message 使用 close code `1009`。
- 长时间空闲连接可能被部署环境关闭，客户端应支持 keepalive。

## Metadata

- `GET /healthz` 返回进程健康状态。
- `GET /readyz` 返回 dataset 已加载状态。
- `GET /metadata` 返回 runtime 数据集规模和格式信息。

## Service Container

Docker 镜像只包含 `wikispine` runtime binary，不包含 `data/runtime/`。镜像声明 `/data/runtime` 为 volume；运行服务镜像时必须把 runtime 数据目录挂载到容器内，否则启动时会因为缺少 `manifest.json` 失败：

```text
WIKISPINE_DATA_DIR=/data/runtime
PORT=9000
```

`wikispine serve` 会优先读取 `WIKISPINE_DATA_DIR` 作为数据目录，优先读取 `WIKISPINE_BIND` 或 `PORT` 作为监听地址。`PORT=9000` 时监听 `0.0.0.0:9000`。

`POST /match` 是完整 JSON request，服务端允许最多 32 MiB request body。更大的输入应拆成多个请求，或使用 WebSocket chunk 流。
