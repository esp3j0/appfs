# AppFS Connector 快速开始

这是当前 AppFS connector 主路径的 quickstart。

请围绕 canonical `AppConnector` 和 managed AppFS runtime 来设计。
进程内 demo connector 是行为参考，HTTP 与 gRPC bridge 需要与其保持一致。

主参考文档：

1. `../../docs/v4/README.md`
2. `../../docs/v4/APPFS-v0.4-AppStructureSync-ADR.zh-CN.md`
3. `../../docs/v4/APPFS-v0.4-Connector结构接口.zh-CN.md`

## 1. 选择 Connector 路径

1. 进程内 connector
2. 进程外 HTTP bridge
3. 进程外 gRPC bridge

## 2. 从当前 Scaffold 开始

生成一个 Python HTTP connector scaffold：

```bash
sh ./new-connector.sh my-app
```

脚手架会生成：

```text
examples/appfs/connectors/my-app/http-python
```

它基于当前 `AppConnector` surface 和当前 bridge contract，而不是旧的 V1 adapter 面。

## 3. 实现 Connector Surface

一个完整 connector 需要定义：

1. `connector_info`
2. `health`
3. `prewarm_snapshot_meta`
4. `fetch_snapshot_chunk`
5. `fetch_live_page`
6. `submit_action`
7. `get_app_structure`
8. `refresh_app_structure`

设计结构和 fixture 时，应先定义：

1. connector-owned 树
2. scope 切换
3. snapshot 资源
4. live pageable 资源
5. action sink

## 4. 用 Live Harness 验证

在本目录执行：

```bash
sh ./run-conformance.sh inprocess
sh ./run-conformance.sh http-python
sh ./run-conformance.sh grpc-python
```

对于你生成的 connector：

```bash
cd connectors/my-app/http-python
uv run python -m unittest discover -s tests -t . -p "test_*.py"
APPFS_ADAPTER_HTTP_ENDPOINT=http://127.0.0.1:8080 sh ./run-conformance.sh
```

## 5. Runtime 模型

推荐的用户入口是：

```bash
agentfs appfs up <id-or-path> <mountpoint>
```

然后通过 `/_appfs/register_app.act` 注册 app。

不要再围绕下面这些旧主路径设计：

1. `AppAdapterV1`
2. 把 `/v1/submit-action` 当成主集成面
3. 把 `/_snapshot/refresh.act` 当成正常 snapshot 读路径
4. 把 `mount + serve appfs` 当成 examples 主流程

## 6. Legacy 参考

历史 v0.1 材料保留在：

1. `legacy/v1/`
2. `../../docs/v1/`
