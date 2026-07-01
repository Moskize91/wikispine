# Builder Pipeline

本文只描述 builder 四个离线步骤的职责、输入输出结构和注意事项，不展开内部实现。

## Why

Builder 的目标不是把 dump 一路“顺手处理”到服务可用，而是把几个性质完全不同的工作拆开：上游数据获取、语义归并、自动机编译、运行时打包。这四类工作在耗时、资源消耗、失败恢复方式和下游依赖上都不一样，混在一个步骤里会让重跑成本失控，也会让 runtime 误依赖中间产物。

`download` 单独存在，是因为 raw dumps 很大、下载很慢，但内容不属于本项目 schema。只要上游文件没有变化，后续可以反复重建而不重新下载。`preprocess` 是语义中心，它把页面标题、重定向、Wikidata label/alias/sitelink 统一成唯一主表 `surface_key -> QID[]`，并确定全局 `surface_id`。这个阶段必须独立，因为它决定后续所有产物的身份空间。

`compile` 只负责编译 surface key 到 Aho-Corasick 自动机，不读取 QID 候选。这样自动机可以按 surface 数量分片，避免一次性编译时内存爆掉；同时它的 output 只保留 `surface_id`，不会把 QID 表结构固化进自动机。这个阶段和内存规模强相关：内存越大，surface 分片可以越大，最终搜索器数量越少，下游每次查询需要跑的自动机也越少。`postprocess` 则把 preprocess 和 compile 的结果合并成 runtime 唯一需要的数据包。这样 runtime 的边界很窄：只读 `data/runtime/`，不关心 dumps、中间 TSV 或 builder 内部自动机格式。

默认数据目录都位于仓库根目录的 `data/` 下，属于生成产物，不应签入 git。四个步骤是：

```text
download -> preprocess -> compile -> postprocess
```

其中 `compile` 只需要 `preprocess` 的 surface 表；`postprocess` 同时需要 `preprocess` 和 `compile` 的产物。

## 1. Download

`download` 负责取得后续构造需要的上游 Wikimedia dump 文件。它只下载和记录文件，不解释 dump 内容，也不生成项目自己的 surface/QID schema。

输入：

- 目标 wiki 列表，默认是 `zhwiki,enwiki`。
- dump 日期，默认是 `latest`。
- dump component 列表，默认包含 `page`、`redirect`、`page_props`、`wikidata_entities`。

输出：

- `data/dumps/manifest.json`
- `data/dumps/<wiki>/<date>/<wiki>-<date>-page.sql.gz`
- `data/dumps/<wiki>/<date>/<wiki>-<date>-redirect.sql.gz`
- `data/dumps/<wiki>/<date>/<wiki>-<date>-page_props.sql.gz`
- `data/dumps/wikidatawiki/<date>/latest-all.json.bz2` 或对应日期的 Wikidata entities dump

注意事项：

- 这是纯上游数据获取步骤；不要在这里做过滤、归并或格式转换。
- `data/dumps/` 只在从原始来源重建 preprocess 时需要，runtime 不读取它。
- 全量下载耗时很长，且文件体积很大；复用已有 dump 时要保持目录结构与 manifest 含义一致。

## 2. Preprocess

`preprocess` 负责把 raw dumps 归并成项目自己的主表：`surface_key -> QID[]`。它同时生成 QID flag 表，目前只记录是否为消歧义页。

`surface_key` 必须使用 `docs/surface-normalization.md` 中定义的标准化规则生成。该规则也是 runtime 查询文本进入自动机前使用的规则。

输入：

- `data/dumps/`
- 目标 wiki 列表和 dump 日期。
- Wikipedia `page`、`redirect`、`page_props` SQL dump。
- Wikidata entities dump。

输出：

- `data/preprocess/manifest.json`
- `data/preprocess/surface_qids.tsv`
- `data/preprocess/qid_flags.tsv`

注意事项：

- `surface_qids.tsv` 是 builder 后续步骤的主表；它的行号定义全局 `surface_id`。
- surface 来源包括 Wikipedia 页面标题、重定向标题、Wikidata label、alias 和 sitelink title。
- 同一个 surface 可以对应多个 QID，因此输出是 `surface_key -> QID[]`，不是一对一映射。
- 本项目不在 preprocess 阶段缩小 QID 规模，也不构建 `P31/P279` 拓扑。
- 消歧义页信息需要完整覆盖 QID，因此 `qid_flags.tsv` 是正式产物，不是顺手调试信息。

## 3. Compile

`compile` 负责把 `surface_qids.tsv` 中的 surface key 编译成 Aho-Corasick 自动机。自动机的 output 是全局 `surface_id`，不直接输出 QID。

输入：

- `data/preprocess/surface_qids.tsv`

输出：

- `data/compile/manifest.json`
- `data/compile/shards/<shard_id>/automaton.bin`
- `data/compile/shards/<shard_id>/manifest.json`

注意事项：

- 编译只关心 surface 字符串和全局 `surface_id`，不读取 QID 候选表。
- 自动机按 `--shard-size` 拆分；查询时需要运行所有 shard 并合并结果。
- `data/compile/` 是 builder 内部中间产物，不是最终 runtime 数据包。
- 全量编译非常吃内存和时间；大规模构造应优先在大内存机器上完成。
- 当前全量数据是专门租用 512GB 内存服务器才完成的，compile 产物来之不易，应在清理本机或云主机前确认已经备份。
- 在可承受失败的前提下，compile 的 `--shard-size` 可以偏激进设置。内存越大，拆出来的搜索器越少；搜索器越少，runtime 查询时的固定开销越低。

## 4. Postprocess

`postprocess` 负责把 preprocess 主表和 compile 自动机整理成 runtime 可以直接读取的数据包。它是 builder 的最终输出步骤。

输入：

- `data/preprocess/surface_qids.tsv`
- `data/preprocess/qid_flags.tsv`
- `data/compile/manifest.json`
- `data/compile/shards/<shard_id>/automaton.bin`

输出：

- `data/runtime/manifest.json`
- `data/runtime/automaton/shards/<shard_id>/char_code_map.bin`
- `data/runtime/automaton/shards/<shard_id>/states.bin`
- `data/runtime/automaton/shards/<shard_id>/state_outputs.bin`
- `data/runtime/surfaces/surface_qid_index.bin`
- `data/runtime/surfaces/surface_qid_values.bin`
- `data/runtime/qids/qid_numbers.bin`
- `data/runtime/qids/qid_flags.bin`

注意事项：

- `data/runtime/` 是 runtime 唯一需要的数据目录。
- runtime 数据包不保存 surface 文本本体；匹配路径依赖自动机输出的 `surface_id`。
- `surface_qid_index.bin` 和 `surface_qid_values.bin` 表达 `surface_id -> QID[]`。
- `qid_numbers.bin` 和 `qid_flags.bin` 表达排序后的 QID flag 表；查询某个 QID flag 时按 QID number 二分。
- postprocess 完成并备份后，`data/dumps/`、`data/preprocess/`、`data/compile/` 都不是 runtime 必需数据。
