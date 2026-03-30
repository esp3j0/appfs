# AppFS Connector 快速开始

这是 AppFS connector 主路径的默认 quickstart。
请以 Rust 进程内 `DemoAppConnector` 作为 canonical 行为面，HTTP/gRPC demo 需要与其一致。

主参考文档：

1. `docs/v4/APPFS-v0.4-AppStructureSync-ADR.zh-CN.md`
2. `docs/v4/APPFS-v0.4-Connector结构接口.zh-CN.md`

## 1. 选择 Connector 路径

1. 进程内 connector（Rust runtime demo 路径）
2. 进程外 HTTP connector bridge
3. 进程外 gRPC connector bridge

## 2. 运行一致性

在该目录执行：

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

脚本会执行：

1. 挂载 AgentFS live 文件系统。
2. 启动 runtime 与所选 transport 路径。
3. 通过 `cli/tests/appfs/run-live-with-adapter.sh` 执行契约测试。

## 3. Canonical Demo 对齐清单

HTTP/gRPC 必须与进程内 canonical 在以下行为面一致：

1. `connector_info` / `health`
2. `prewarm_snapshot_meta`
3. `fetch_snapshot_chunk`
4. `fetch_live_page`
5. `submit_action`

只允许以下 transport 差异：

1. `connector_id`
2. `transport`
3. 传输 envelope 细节

关键 parity fixture：

1. snapshot start：`rk-001/rk-002`；cursor 跟进（`cursor-2`）：`rk-003`
2. snapshot `emitted_bytes`：紧凑 JSON 行字节 + 换行（`+1`）累计
3. live paging：`handle_id=demo-live-handle-1`，`cursor-1` 推进
4. inline submit：`{"ok":true,"path":"...","echo":<payload>}`
5. streaming submit：accepted `{"state":"accepted"}`、progress `{"percent":50}`、terminal `{"ok":true}`

## 4. HTTP / gRPC 起步目录

1. HTTP：`examples/appfs/http-bridge/python/`
2. gRPC：`examples/appfs/grpc-bridge/python/`（启动前执行 `./generate_stubs.sh`）

## 5. Legacy 参考（v0.1）

v0.1 `AppAdapterV1` 文档与模板仅保留为 legacy/reference：

1. `../../docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md`
2. `examples/appfs/adapter-template/rust-minimal`
