# 火眼对接说明

## 当前状态

火眼已经有一版可运行的 HTTP bridge backend，代码位置：

- [huoyan_backend.py](/C:/Users/esp3j/.codex/worktrees/0052/agentfs/examples/appfs/bridges/http-python/appfs_http_bridge/huoyan_backend.py)
- [server.py](/C:/Users/esp3j/.codex/worktrees/0052/agentfs/examples/appfs/bridges/http-python/appfs_http_bridge/server.py)
- [test_huoyan_backend.py](/C:/Users/esp3j/.codex/worktrees/0052/agentfs/examples/appfs/bridges/http-python/tests/test_huoyan_backend.py)

这版已经支持：

- `home` scope 下展示案件列表
- `enter_scope -> case:<cid>` 进入案件
- `enter_scope -> home` 退出案件并回到首页
- 基于节点树和检材列表构造案件目录结构
- 叶子数据按 snapshot 方式读取

## 关键接口

- 案件列表：`GET /api/v1/cases`
- 运行参数：`GET /internal/v1/app/options`
- 检材列表：`GET <storagehost>/internal/v1/evidence/cid?cid=<real_cid>`
- 树节点：`GET /api/v1/data/node?cid=<analysis_cid>&pid=<nid>`
- 叶子数据：优先 `GET <storagehost>/internal/v1/data?...`，必要时回退 `GET /api/v1/data?...`
- 进入案件：`POST /api/v1/case/open`，body 传小写 `path`
- 退出案件：`POST /api/v1/case/exit`

## 单机版 / 平台版差异

- 单机版分析 API 使用 `analysis_cid = 1`
- 平台版分析 API 使用真实案件 `cid`
- 检材接口始终使用真实案件 `cid`

## 详细设计

- [nodes.md](/C:/Users/esp3j/.codex/worktrees/0052/agentfs/app_documents/huoyan/nodes.md)
