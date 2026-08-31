# 隐私说明（Privacy）

小狗桌宠在你的电脑上本地运行。**它不收集、不上传任何数据，不包含遥测、统计或广告 SDK。**

## 处理哪些数据

为了显示会话状态，桌宠会处理以下信息，且全部只存在于本机内存中：

- 会话来源（Claude Code / Codex / OpenCode / ZCode）
- 会话名称/标题
- 会话状态（运行中、等待输入、已完成、出错）与时间戳

唯一持久化的数据是**静音会话列表**（`muted-sessions.json`，保存在应用数据目录），用于记住你手动静音的会话，同样只保存在本地。

## 只读访问哪些本地文件

桌宠会**只读**查询以下文件来推断会话状态（不修改它们）：

| 工具 | 只读访问 |
| --- | --- |
| Codex | `~/.codex/thread_history_1.sqlite`、`~/.codex/state_5.sqlite`、thread 对应的 rollout JSONL |
| Claude Code | `~/.claude/projects` 下的会话 transcript JSONL（仅读取会话标题；路径经过规范化校验） |
| OpenCode | `~/.local/share/opencode/opencode.db`（或 `%XDG_DATA_HOME%` 指定位置，仅 session 表） |
| ZCode | `~/.zcode/cli/db/db.sqlite`、`~/.zcode/cli/log/zcode-*.jsonl` |

## 写入哪些本地文件

首次启动时会安装监测桥接（详见 [README](README.md) 的配置表格）：

- `~/.codex/hooks.json`（或 `%CODEX_HOME%\hooks.json`）
- `~/.claude/settings.json`
- `~/.zcode/cli/config.json`
- `~/.config/opencode/plugins/golden-pet/pet-bridge.ts`（新建文件）

修改 Codex、Claude、ZCode 的现有 JSON 前会在同目录生成 `.golden-puppy.bak` 备份；只管理本项目自己的条目，不影响你的其他配置。

## 网络行为

- 桌宠在本地启动一个事件服务，**只监听 `127.0.0.1:7878`**，不接受局域网或互联网连接。
- 各工具的 Hook/插件把会话事件发送到这个本地地址（含会话名等字段），事件仅在本机进程之间流动。
- 除此之外没有任何网络请求；不会把会话内容、使用统计或任何标识信息发送给任何人。

## 已知限制

- 本地事件端点（127.0.0.1:7878）目前没有认证：本机上的其他进程理论上可以向它发送伪造事件（最多造成一条假的会话提醒）。它无法被外部网络访问。
- 卸载程序不会自动还原上述四个工具的配置，手动清理步骤见 [README](README.md) 的「卸载与手动清理 Hook」。

如有隐私方面的疑问或问题，请通过 [GitHub Issues](../../issues) 反馈。
