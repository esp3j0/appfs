# AppFS 适配器快速开始（MVP）

这份文档面向希望以最小配置通过 AppFS v0.1 一致性测试的适配器开发者。

完整实现细节与排障请看：

1. `docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md`
2. `docs/v1/APPFS-adapter-structure-mapping-v0.1.zh-CN.md`

## 1. 选择适配路径

1. 进程内（Rust runtime demo 路径）：
   - 跑完整 live 套件最快。
2. 进程外 HTTP bridge：
   - 便于多语言集成。
3. 进程外 gRPC bridge：
   - 传输契约更强类型化，适合多语言团队。

## 2. 一键一致性

在该目录执行：

```bash
cd examples/appfs
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

脚本会执行：

1. 挂载 AgentFS live 文件系统。
2. 启动适配器 runtime（或 runtime + bridge endpoint）。
3. 通过 `cli/tests/appfs/run-live-with-adapter.sh` 执行 `CT-001` 到 `CT-017`。

## 3. 写代码前先定义结构

先完成三件事：

1. 在 `manifest.res.json` 声明节点模板（`*.res.json`、`*.act`）。
2. 在 `/app/<app_id>/...` 落地对应 sink/resource 文件。
3. 建立“节点模板 -> handler”映射表。

参考：

1. `../../docs/v1/APPFS-adapter-structure-mapping-v0.1.zh-CN.md`

## 4. 最小 Rust 适配器模板

模板位置：

1. `examples/appfs/adapter-template/rust-minimal`

模板命令：

```bash
cd examples/appfs/adapter-template/rust-minimal
cargo test
```

模板测试使用冻结的 SDK 矩阵 runner：

1. `run_required_case_matrix_v1`
2. `run_error_case_matrix_v1`

## 5. HTTP Bridge 起步

入口位置：

1. `examples/appfs/http-bridge/python/bridge_server.py`
2. `examples/appfs/http-bridge/python/run-conformance.sh`

手动启动：

```bash
cd examples/appfs/http-bridge/python
uv run python bridge_server.py
```

## 6. gRPC Bridge 起步

入口位置：

1. `examples/appfs/grpc-bridge/python/grpc_server.py`
2. `examples/appfs/grpc-bridge/python/run-conformance.sh`

运行 gRPC quickstart 前：

1. 安装 `examples/appfs/grpc-bridge/python/requirements.txt` 依赖。
2. 执行 `./generate_stubs.sh` 生成 stubs。

## 7. 兼容性最小检查清单

声明兼容前，请确认：

1. `.act` 的 `write+close` 提交语义正确。
2. 流生命周期与重放面正确。
3. 分页句柄错误映射（`fetch_next`、`close`）正确。
4. `AppAdapterV1` 契约符合规范。
5. CI/static/live 一致性证据齐全。
6. 声明节点与桥接 handler 1:1 映射完成。

参考文档：

1. `../../docs/v1/APPFS-v0.1.md`
2. `../../docs/v1/APPFS-adapter-requirements-v0.1.zh-CN.md`
3. `../../docs/v1/APPFS-compatibility-matrix-v0.1.zh-CN.md`
4. `../../docs/v1/APPFS-conformance-v0.1.zh-CN.md`
5. `../../docs/v1/APPFS-contract-tests-v0.1.zh-CN.md`
6. `../../docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md`
7. `../../docs/v1/APPFS-adapter-structure-mapping-v0.1.zh-CN.md`

## 8. 排障入口

如遇 runtime/bridge 测试失败（端口冲突、`uv`、gRPC 依赖、CT-017），先看：

1. `../../docs/v1/APPFS-adapter-developer-guide-v0.1.zh-CN.md#8-常见问题排障`

## 9. 生成新适配器脚手架

生成一个新的 Python HTTP bridge 脚手架：

```bash
sh ./new-adapter.sh myapp
```

生成目录：

1. `./adapters/myapp/python`

如果要使用自定义 app fixture 跑 live 测试，覆盖以下环境变量：

1. `APPFS_FIXTURE_DIR`
2. `APPFS_APP_ID`
