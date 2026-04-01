# background

这是一个取证软件，叫做火眼。页面服务端口会变化，可以通过当前运行实例的页面端口访问：

- 页面接口：`http://127.0.0.1:<page_port>`
- 并且可以通过 `GET /internal/v1/app/options` 动态发现真实 `storagehost`

一个火眼软件中可以存放多个案件，每个案件中有多个检材（电子证据）。

这个软件的首页是案件列表页面，点击案件后进入案件详情页面，这个页面用于分析取证。我期望 app 挂载的时候，可以挂载首页，使用 `.act` 可以进入对应的案件并刷新目录结构。我还期望可以直接挂载指定的案件。

需要注意当前 AppFS runtime 的现实约束：

- 第一版真正可用的结构切换入口是 `/_app/enter_scope.act`
- 因此首页 scope 中每个案件目录可以暴露 `info.res.jsonl`
- “进入案件”的主路径应优先实现为向 `/_app/enter_scope.act` 提交 `{"target_scope":"case:<cid>"}`，而不是直接依赖 `案件名/enter.act`

当前已经确认的接口事实：

- 案件列表：`GET /api/v1/cases?limit=<n>&offset=<n>&desc=true&column=update_at&keyword=`
- App 运行参数：`GET /internal/v1/app/options`
- 检材列表：`GET <storagehost>/internal/v1/evidence/cid?cid=<real_cid>`
- 树节点：`GET /api/v1/data/node?cid=<analysis_cid>&pid=<nid>`
- 叶子数据：优先 `GET <storagehost>/internal/v1/data?...`，必要时回退 `GET /api/v1/data?...`
- 进入案件：`POST /api/v1/case/open`，body 传小写 `path`
- 退出案件：`POST /api/v1/case/exit`
- 鉴权参数名：`getoken`

单机版和平台版的唯一区别：

- 单机版进入案件后，分析 API 使用 `analysis_cid = 1`
- 平台版分析 API 使用真实案件 `cid`

---

# AppFS 目标挂载结构

## 1. 首页 scope

首页应作为一个可挂载 scope 暴露案件列表。建议结构如下：

```text
/
├── _app/
│   ├── enter_scope.act
│   └── refresh_structure.act
├── 案件名称1/
│   └── info.res.jsonl
├── 案件名称2/
│   └── info.res.jsonl
└── ...
```

语义约定：

- `info.res.jsonl`：案件基础信息快照，例如案件名称、案件 id、创建时间、案件状态、检材数量
- `/_app/enter_scope.act`：进入该案件，`target_scope` 取值形如 `case:<cid>`

## 2. 案件分析页 scope

进入案件后，根目录切换为该案件分析树。理想结构如下：

```text
/
├── 检材名称A/
│   ├── 微信/
│   │   ├── 用户名A(wxid_xxxx)/
│   │   │   ├── 账户信息.res.jsonl
│   │   │   ├── 好友消息/
│   │   │   │   └── 好友A.res.jsonl
│   │   │   └── ...(其他数据)
│   │   └── 用户名B(wxid_xxxx)/
│   ├── 文件系统/
│   ├── 位置信息/
│   └── ...(其他软件)
└── ...(其他检材)
```

第一版的重点不是一次覆盖全树，而是先保证：

- 检材层正确
- App 节点层正确
- 进入具体 app/账号后，关键叶子节点可稳定映射为可读资源文件

---

# 火眼真实后端树模型

下面这些内容来自已有 MCP / 树构建逻辑和已确认接口，后续 connector 应优先兼容这些规则。

## 1. 检材层不是后端天然返回的

案件分析页中的“检材目录”需要由 connector 按 `eid` 分组后插入。也就是说：

```text
/
└── 检材名称A/
    └── 微信/
```

这层“检材目录”是结构化重组结果，不是节点接口天然就有的一层。

## 2. 原始分析树来自 `/api/v1/data/node`

树遍历时调用：

- `/api/v1/data/node?cid=<analysis_cid>&pid=<node.nid>`

主要字段：

- `Nid`
- `Pid`
- `Eid`
- `Tid`
- `Id`
- `Name`
- `HasChildNode`
- `SubNodeType`

当前判定规则：

- `HasChildNode != 1` 视为叶子节点
- 叶子节点的 `data_type = SubNodeType`
- 非叶子节点继续展开，除非命中 `blocked_names`

## 3. 检材列表来自 `<storagehost>/internal/v1/evidence/cid`

这个接口是正式数据源之一，不应该只依赖节点树接口来恢复检材层。

## 4. 文件系统和位置信息都是虚拟插入层

建议在 connector 中显式保留两类虚拟节点：

- `文件系统`
- `位置信息`

原因是它们的数据来源与普通 app 节点不同：

- `文件系统`：走 `/api/v1/data?datatype=file`
- `位置信息`：走固定虚拟节点映射

## 5. 叶子节点统一映射成 AppFS 资源文件

不沿用旧树逻辑里的 `.csv` 命名，统一映射成 AppFS 资源文件：

- 结构化快照：`*.res.jsonl`
- 单对象快照：`*.res.json`
- 控制动作：`*.act`

例如：

- `账户信息` -> `账户信息.res.jsonl`
- 某个好友或会话消息 -> `好友A.res.jsonl`

---

# 推荐的数据源与 connector 方法映射

## `get_app_structure`

返回：

- 当前 scope 下的目录树
- 当前 revision
- 当前 active scope

数据来源：

- 首页：案件列表接口
- 案件分析页：`/internal/v1/evidence/cid` + `/api/v1/data/node` + 虚拟节点规则

## `refresh_app_structure`

支持两类刷新：

- `enter_scope`
- `refresh_structure`

已确认的切换行为：

- `target_scope=case:<cid>`：进入案件
- `target_scope=home`：退出当前案件并回到首页

## `fetch_snapshot_chunk`

用于：

- 账户信息
- 会话消息
- 好友列表
- 其他适合做冷读快照的数据

## `submit_action`

第一版动作面保持极小：

- 结构控制继续走 runtime 内建的 `/_app/enter_scope.act`
- 火眼 backend 暂不暴露业务 `.act`

---

# 第一版最小闭环

建议第一版火眼 connector 的闭环范围固定为：

- 首页案件列表
- 进入案件
- 检材层
- 1 个主 app（例如微信）
- 账号层
- 账户信息
- 1 类消息记录

后续再扩：

- 文件系统
- 位置信息
- 其他 app
- 更深叶子类型

---

# 代码入口

当前火眼 backend 代码在：

- `examples/appfs/bridges/http-python/appfs_http_bridge/huoyan_backend.py`
- `examples/appfs/bridges/http-python/appfs_http_bridge/server.py`
- `examples/appfs/bridges/http-python/tests/test_huoyan_backend.py`

如后续要继续补充接口抓包、字段映射或 scope 设计，直接在这个目录下追加文档即可。
