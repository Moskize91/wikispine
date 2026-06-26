# Runtime API

Runtime 是一个读取 `data/runtime/` 的服务进程。它提供两种入口：HTTP 完整输入和 WebSocket 双向流。两者输出相同的 match event。

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
- `start` 和 `end` 是 UTF-16 offset，和 JavaScript 字符串索引一致。
- 当前输出顺序按 automaton shard 扫描顺序产生，不承诺全局按 offset 排序。

## WebSocket Match

`GET /match` 可以升级为 WebSocket。WebSocket 用于临时性的双向流：客户端分 chunk 发送文本，服务端边接收边返回 match event。

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
{"type":"ack","received_chars":4}
{"type":"done","stats":{"matches":1}}
```

注意事项：

- WebSocket 连接是临时连接，客户端必须能处理断开和重连。
- 服务端按连接维护 automaton state，因此可以识别跨 chunk 的 surface。
- `end` 会结束当前输入流并返回 `done`；之后连接仍可继续发送新的 `start/chunk/end` 序列。
- 长时间空闲连接可能被部署环境关闭，客户端应支持 keepalive。

## Metadata

- `GET /healthz` 返回进程健康状态。
- `GET /readyz` 返回 dataset 已加载状态。
- `GET /metadata` 返回 runtime 数据集规模和格式信息。
