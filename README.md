# 小狗桌宠（Puppy Desktop Pet）

一只趴在 Windows 桌面角落的原创像素小狗，实时帮你盯住多个 AI 编程工具的任务状态：**运行中、等待输入、已完成、出错**，不用来回切窗口，一眼便知。

> A tiny original pixel-art puppy that lives on your Windows desktop and tracks the task status of Claude Code, OpenCode, Codex and ZCode sessions — running, waiting for input, completed, or failed — so you don't have to keep switching windows. All data is processed locally; nothing is uploaded. See the English summary at the end of this README.

## 支持的工具与平台

| 监测对象 | CLI | 桌面端 | 监测方式 |
| --- | --- | --- | --- |
| Claude Code | ✅ | ✅ | HTTP Hook + 会话标题读取 |
| Codex | ✅ | ✅ | Hook + 本地数据库/rollout 文件监测 |
| OpenCode | ✅ | ✅ | 自动安装的 TypeScript 插件 |
| ZCode | ✅ | ✅ | Hook + 数据库轮询 + 桌面日志增量监测 |

- 操作系统：**仅 Windows 10 / 11 x64**（macOS 暂未发布）
- 兼容性提示：监测依赖各工具当前版本的本地 Hook 格式与数据文件结构，上游更新格式后可能失效，届时请提 [Issue](../../issues) 反馈。

## 功能

- 16×16 原创像素小狗（48×48 显示），带状态动画：工作、等待、完成、休眠。
- 多会话面板：显示程序名、会话名、状态与持续时间；面板内部滚动，宽度固定不抖动。
- **等待输入**红字提醒：权限请求、问答、交互请求会立刻标红。
- **已完成**提醒保留，直到你确认或清除。
- 行尾箭头：一键跳到对应程序窗口（保持目标窗口原有的最大化状态）。
- 行首叉号：静音误报会话，直到该会话出现新活动前不再打扰。
- 小狗本体支持键盘操作：`Tab` 聚焦，`Enter` / `Space` 开关面板。
- 系统托盘：显示/隐藏、开机自启、退出。
- 点击已完成状态可快速确认提醒。

## 状态含义

| 状态 | 含义 |
| --- | --- |
| 🐕 运行中 | 会话正在执行任务，面板显示已持续时间 |
| ❗ 等待输入（红字） | 需要你回答问题、授权或交互 |
| ✅ 已完成 | 任务结束，提醒保留至你清除 |
| ⚠️ 出错 | 任务失败，需要检查 |
| 😴 休眠 | 没有任何活跃会话 |

## 安装

1. 从 [Releases](../../releases) 下载最新版 `golden-puppy-pet_<版本>_Windows_x64.exe`（即「小狗桌宠」的 Windows x64 安装包）。
2. （推荐）用 PowerShell 校验 SHA-256，与 Release 页面公布值比对：
   ```powershell
   Get-FileHash .\golden-puppy-pet_0.4.1_Windows_x64.exe -Algorithm SHA256
   ```
3. 运行安装包。升级时直接覆盖安装即可，无需先卸载。
4. 安装后启动一次「小狗桌宠」，它会按当前 Windows 用户与实际安装位置自动安装各工具的监测桥接（见下一节）。如果相关工具当时已经打开，请重启对应程序让桥接生效。

### SmartScreen 提示

安装包**未使用商业代码签名证书**，首次运行可能触发 Windows SmartScreen「未知发布者」警告。这是未签名的正常提示，不代表安装包损坏。请以 Release 页面公布的 SHA-256 核对文件后再继续（「更多信息」→「仍要运行」）。请只从本仓库 GitHub Release 下载，不要使用来路不明的镜像。

## 首次启动会修改哪些配置（重要）

为了接收状态事件，桌宠启动时会自动写入以下四个工具的配置。**这是本项目的核心工作机制，在此完全公开说明：**

| 工具 | 修改的文件 | 说明 |
| --- | --- | --- |
| Codex | `~/.codex/hooks.json`（或 `%CODEX_HOME%\hooks.json`） | 添加指向本桌宠的事件 Hook |
| Claude Code | `~/.claude/settings.json` | 添加指向本机服务的 HTTP Hook |
| OpenCode | `~/.config/opencode/plugins/golden-pet/pet-bridge.ts` | 新建插件文件（仅由本项目管理） |
| ZCode | `~/.zcode/cli/config.json` | 启用 hooks 并添加本桌宠的 process hook |

行为约定：

- 修改 Codex、Claude、ZCode 的现有 JSON 前，会在**同目录**生成 `.golden-puppy.bak` 备份。
- 只新增/替换本项目自己管理的条目，**保留**你已有的其他 Hook、主题、MCP 配置。
- 重复启动是幂等的，不会堆积重复条目。
- 所有事件都发送到本机 `127.0.0.1:7878`，不访问外部网络。

## 使用方法

- 桌宠默认置顶显示在桌面；点击小狗（或 `Enter`/`Space`）开关会话面板。
- 面板中每行：叉号 = 静音该会话；箭头 = 跳到对应程序窗口；完成行右侧勾 = 清除提醒。
- 托盘图标右键：显示/隐藏、开机自启、退出。

## 卸载与手动清理 Hook

卸载程序不会自动还原上述四个工具的配置。如需彻底清理：

1. **Codex / Claude / ZCode**：打开对应配置文件，删除命令中包含 `golden-puppy-pet.exe` 或 `--pet-hook` 的 Hook 条目；也可以用同目录的 `.golden-puppy.bak` 备份直接恢复（注意备份是安装桥接前的快照，恢复会同时丢失其后你自行添加的其他改动）。
2. **OpenCode**：删除目录 `~/.config/opencode/plugins/golden-pet/`。
3. （可选）删除应用数据目录中的 `muted-sessions.json`（静音记录）。

## 隐私

- 所有会话状态数据仅在本机进程之间流动，**无遥测、无云端上报、不收集任何数据**。
- 本地事件服务只监听 `127.0.0.1`，不接受局域网/互联网请求。
- 桌宠会**只读**访问各工具的本地数据库与日志文件以推断会话状态，详情见 [PRIVACY.md](PRIVACY.md)。

## 已知限制

- 仅支持 Windows；macOS 未验证、未发布。
- 状态判定依赖各工具本地数据格式，上游更新可能导致漏报/误报。
- 本地事件端点（127.0.0.1:7878）暂无认证，本机其他进程理论上可伪造事件。
- 「跳转窗口」按程序选择最佳匹配窗口，同一程序开多个窗口时不保证精确到具体会话。
- 无自动更新，升级需下载新安装包覆盖安装。
- 若 7878 端口被占用，事件服务可能启动失败。
- 长时间无事件的活动会话（默认 10 分钟）会被自动清理。

## 从源码构建

环境要求：Windows 10/11、Node.js 18+、Rust stable（MSVC 工具链）。

```powershell
npm ci
npm run tauri -- build        # 产物位于 src-tauri/target/release/bundle/nsis/
```

开发与测试：

```powershell
npm run tauri -- dev          # 开发模式
npm run typecheck             # TypeScript 类型检查
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

## 目录结构

```text
src/                     前端：TypeScript + Canvas 像素小狗与面板渲染
  main.ts                  面板状态与交互
  sprite.ts                16×16 像素矩阵与状态动画
  styles.css               透明窗口与面板样式
src-tauri/
  src/lib.rs               Rust 后端：状态存储、监测器、本地服务、托盘
  src/integrations.rs      四种工具的 Hook/插件自动安装
  resources/
    opencode-pet-bridge.ts OpenCode 插件模板
  tauri.conf.json          窗口与打包配置
```

## 免责声明

这是一个非官方的第三方个人项目，与 OpenAI、Anthropic、OpenCode、ZCode 及其关联公司不存在隶属、授权或背书关系。文中出现的产品名称仅用于说明兼容性，其商标归各自权利人所有。本项目未使用上述任何一方的官方 Logo 或素材；小狗形象为本项目原创的像素绘制。

## License

[MIT](LICENSE)

---

## English Summary

**Puppy Desktop Pet** (小狗桌宠) is a Windows-only (10/11 x64) desktop pet — an original 16×16 pixel puppy — that monitors the session status of **Claude Code, Codex, OpenCode and ZCode** (both CLI and desktop apps) and shows at a glance whether tasks are *running*, *waiting for your input*, *completed*, or *failed*.

- Everything runs **locally**: a loopback-only event endpoint on `127.0.0.1:7878`, no telemetry, no cloud uploads.
- On first launch it transparently installs hooks/plugins for the four tools listed above (backup files are created; see the configuration table for details and manual removal steps).
- The installer is **not code-signed**; verify the SHA-256 checksum published on the Release page before running.
- Monitoring depends on the current local data formats of those tools and may break when they change — please open an issue if that happens.

Licensed under [MIT](LICENSE).
