这个仓库是 `wikispine` 的开源项目仓库，对应 <https://github.com/Moskize91/wikispine>。

项目目标是把 Wikipedia/Wikidata 的 surface text 编译成本地可查询的实体候选召回数据集。项目只围绕两块工程边界组织：`builder` 负责离线构造数据，`runtime` 负责加载构造好的数据并提供查询服务。

# 现状总览

- 当前仓库采用 Rust workspace 结构。
- `crates/builder/` 是离线构造器，负责从 Wikimedia dump 生成 `data/runtime/`。
- `crates/runtime/` 是发布给用户的 `wikispine` CLI，负责消费 `data/runtime/`，提供本地 pipe 查询、初始化安装和 HTTP/WebSocket 服务 API。
- `docker/Dockerfile` 只面向 runtime 服务镜像。builder 不以 Docker 作为主要交付形态。
- `data/` 下的所有内容都是生成产物，不应签入 git。

# 架构边界

- Builder 关注下载、预处理、自动机编译、运行时数据打包。它可以消耗大量 CPU、内存、磁盘和时间。
- Runtime 关注稳定读取 `data/runtime/` 并对输入文本执行 surface 匹配和 QID 候选查询。它不应依赖 raw dumps、preprocess 或 compile 中间产物。
- Runtime 数据包是 builder 和 runtime 之间的实际契约。当前不单独维护 format crate。
- 项目不维护 EID 中间层。实体标识直接使用 Wikidata QID number。
- 当前只保留 QID 的消歧义页 flag，不构建 `P31/P279` 类型拓扑。

# 文档路由

- 涉及 builder 的 `download`、`preprocess`、`compile`、`postprocess` 四步职责和输入输出边界时，阅读 `docs/builder-pipeline.md`。
- 涉及 runtime 服务加载、HTTP/WebSocket 查询 API、Docker 运行方式时，阅读 `docs/runtime-api.md`。

# 文档原则

- 根 `AGENTS.md` 是 AI 路由表，不是完整设计文档。
- 下层文档负责展开具体边界；上层文档只描述应该去哪里读。
- 文档只写项目特有约束和容易被带偏的决策，不重复通用 Rust、Docker 或 Wikimedia 常识。
- 更新代码结构时，同步更新本文的文档路由和架构边界。
