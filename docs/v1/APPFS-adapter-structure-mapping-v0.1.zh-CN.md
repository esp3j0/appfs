# AppFS 适配器结构定义与桥接映射指南 v0.1（中文）

- 版本：`0.1`
- 日期：`2026-03-18`
- 状态：`Draft`
- 读者：接入真实应用后端的适配器开发者

## 1. 为什么需要这份文档

开发者最常见的两个困惑是：

1. “AppFS 结构怎么定义（首页/设置页这些到底怎么落文件）？”
2. “生成脚手架后，文件怎么和 bridge handler 对上？”

这份文档就是把这两件事讲透。

## 2. 三层真相源（Source of Truth）

真实 app 适配器建议固定三份工件，各司其职：

1. `manifest.res.json`（契约真相源）
2. AppFS 文件树（runtime 可见面）
3. Bridge 路由映射表（实现真相源）

顺序必须是：

1. 先在 manifest 设计节点模板。
2. 再落地文件树中的实际 `.act`/`.res.json`。
3. 最后为每个声明节点实现 bridge handler。

## 3. 如何定义 AppFS 结构

## 3.1 “页面”到“路径”的转换

AppFS 是能力模型，不是 UI 截图模型。  
页面要拆成 agent 可读/可写的能力节点。

示例：

1. 首页信息流 -> `home/feed.res.json`
2. 设置页保存资料 -> `settings/profile/save.act`
3. 设置页开关通知 -> `settings/notifications/toggle.act`

不要把整页做成一个黑盒文件，要把可操作能力拆出来。

## 3.2 节点模板规则

在 `nodes` 里声明模板路径：

1. live 资源：`*.res.json`
2. snapshot 全量文件资源：`*.res.jsonl`
3. 动作：`*.act`
4. 动态实体用占位符（如 `{user_id}`、`{chat_id}`）

示例：

```json
{
  "nodes": {
    "home/feed.res.json": { "kind": "resource", "output_mode": "json" },
    "chats/{chat_id}/messages.res.jsonl": {
      "kind": "resource",
      "output_mode": "jsonl",
      "snapshot": { "max_materialized_bytes": 10485760 }
    },
    "settings/profile/save.act": {
      "kind": "action",
      "input_mode": "json",
      "execution_mode": "inline",
      "input_schema": "_meta/schemas/settings.profile.save.input.schema.json"
    }
  }
}
```

## 3.3 运行时关键行为（必须知道）

当前 runtime 规则：

1. 从 `_meta/manifest.res.json` 加载 action 规格。
2. 将 `/app/<app_id>/...` 下的 `*.act` 视为 append-only JSONL sink，并维护每个 sink 的游标偏移。
3. 按观测顺序提交每条“以换行结尾”的完整 JSON 行。
4. 尾部不完整行（无 `\n`）延迟到补齐后再提交。
5. 未在 manifest 声明的 `.act` 会被忽略（无副作用）。

直接结论：

1. 只写 manifest 不创建 `.act` 文件，动作触发不了。
2. 只创建 `.act` 文件不写 manifest，runtime 会忽略。

## 4. 脚手架和文件怎么关联

生成脚手架后，建议维护 1:1 映射表：

1. 一个 action 模板 -> 一个桥接处理分支
2. 一个控制动作 kind（`paging_fetch_next` / `paging_close`）-> 一个控制处理函数（仅 live 分页）
3. 一个 snapshot 刷新动作（`_snapshot/refresh.act`）-> 一个 snapshot 物化处理函数
4. 一个 resource 模板 -> 一个资源生产逻辑

推荐表格：

| 节点模板 | 类型 | 执行模式 | Bridge 路由 | 后端处理函数 |
|---|---|---|---|---|
| `contacts/{contact_id}/send_message.act` | action | inline | `/v1/submit-action` | `handle_send_message` |
| `files/{file_id}/download.act` | action | streaming | `/v1/submit-action` | `handle_download` |
| `_snapshot/refresh.act` | action | inline | `/v1/submit-action` | `handle_snapshot_refresh` |
| `_paging/fetch_next.act` | control | inline | `/v1/submit-control-action` | `handle_paging_fetch_next` |
| `_paging/close.act` | control | inline | `/v1/submit-control-action` | `handle_paging_close` |

## 5. Bridge 实现建议分层

建议固定三层：

1. `protocol.py`：校验、分发、错误映射
2. `mock_aiim.py` 或真实连接器：业务逻辑
3. 可选路由契约文件：模板匹配与 handler 注册

每个 action 模板都要做到：

1. 按 `input_mode` 和 schema 意图校验 payload。
2. 严格对齐 `execution_mode`（`inline` / `streaming`）。
3. 返回 AppAdapterV1 兼容结果（`completed` 或 `streaming plan`）。

## 6. 真实 app 连接器最小实施顺序

1. 从产品流程提炼“能力清单”（不是页面名）。
2. 在 `manifest.res.json` 声明节点模板和 schema。
3. 在 AppFS 文件树创建对应 sink/resource 文件。
4. 补齐节点到 handler 的映射表。
5. 在 bridge backend 实现 handler。
6. 跑测试：
   - 协议层/后端单测
   - `CT-001 ~ CT-022` live 一致性（开启 bridge 韧性探针时执行 `CT-017`）

## 7. 常见失败模式

1. 模板与实际路径不匹配 -> 动作被忽略。
2. `input_mode=json` 但按文本处理 -> 提交时拒绝或后端报错。
3. `execution_mode` 实现不一致（例如应 inline 却做成 streaming）-> 合约失败。
4. 声明 live 可分页资源却缺 `_paging/*` 动作 -> 一致性失败。
5. 声明 `output_mode=jsonl` 却未给 `snapshot.max_materialized_bytes` -> manifest 策略失败。

## 8. 这类问题该改文档还是改代码

这类困惑需要“文档 + 脚手架代码”一起解决：

1. 文档必须先补，明确结构定义与映射流程。
2. 脚手架建议生成映射模板，减少开发者自行猜测成本。

本仓库按这个顺序推进：先文档收敛，再脚手架增强。
