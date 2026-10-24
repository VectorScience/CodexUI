# 会话同步与导入

本功能针对本机 Codex rollout 会话，通过完整历史文件迁移上下文。数据存储保持本地，不依赖 Supabase 或云端服务。需要 CodexUI、可用的本机 Codex CLI，以及两台电脑之间已有的目录同步软件。云端聊天和没有本地 rollout 的会话不在此入口的支持范围内。

## 使用流程

电脑 A 在 CodexUI 中连接 Codex，并点击「启用 Codex 右键菜单」。右键侧栏中的会话，选择「同步到目录」，原生目录选择器会让你选择目标目录。原菜单里的选项会保留；当桌面客户端没有渲染可扩展菜单时，CodexUI 会显示独立菜单。CodexUI 自身的会话列表提供同样的同步入口，不依赖桌面客户端的 DOM 结构。

第一次同步成功后，CodexUI 会记住绑定，每 15 秒检查源会话。源文件没有变化时跳过重建，发生变化后才生成新快照。进程重新启动后自动恢复同步绑定；右键菜单在下一次连接 Codex 时自动恢复。多个 Codex 主窗口分别连接，不向透明辅助窗口注入。

由同步软件将整个 `codex-session-UUID` 目录同步到电脑 B。在电脑 B 的右键菜单或 CodexUI 内选择「导入会话」，选择该迁移包目录，再选择这台电脑上对应的项目目录，确认后导入。电脑 B 不需要原始项目路径与电脑 A 相同。

导入总是创建新会话，不覆盖已有聊天。重复导入同一快照也会得到另一个独立副本。两台电脑分别继续聊天后，本功能不会自动合并分叉内容。停止同步只移除本机绑定，保留目录中已有的迁移包。

选择主会话时，会递归收集它的所有层级子 Agent，以及它们引用的父会话、fork 来源和历史分页。预览分别显示逻辑会话数、子 Agent 数和历史文件数。一个会话可以包含多个历史文件；列表只显示该会话的最新页。同步会检测新增子 Agent，无关聊天的更新不会使当前快照失效。

## 迁移包格式

```text
codex-session-<源会话 UUID>/
  CURRENT                         # 最新快照的 SHA-256
  snapshots/
    <清单 SHA-256>/
      bundle.json                 # 格式版本、根会话、标题、源项目、文件校验信息
      <源 UUID>.jsonl.zst          # Zstandard 压缩的完整 rollout
      <历史依赖 UUID>.jsonl.zst
```

快照先写到临时目录，完成后发布，再原子更新 `CURRENT`。历史快照不可变，旧快照会保留，便于同步软件传输和人工恢复。长期同步很长的会话会积累空间；清理前应先停止该绑定，并保留 `CURRENT` 指向的快照。

新导出使用格式 v2：清单的 `threads` 将逻辑会话 UUID 映射到最新分页的文件 UUID；`entries` 使用文件 UUID，并保留分页文件的时间顺序。同一逻辑会话的多个分页允许共享 `session_meta.id`。导入仍兼容格式 v1；旧版 CodexUI 不能导入 v2 包，需要先更新。

导入验证清单 SHA-256、解压后大小、每个文件 SHA-256、JSON、会话 ID、完整依赖链与路径边界。同步软件尚未传完文件、内容损坏或出现循环依赖时停止导入，不创建会话。单个文件上限为解压后 512 MiB，总量上限 1 GiB，最多 256 个历史文件。

## UUID、日期和路径

- 每个导入的历史文件分配新 UUID，保留依赖关系。源会话文件保持原样。
- 逻辑会话和分页文件分别映射 ID。保留并重映射 `source.subagent.thread_spawn.parent_thread_id`、`forked_from_id`、`session_id` 和历史分页关联；Agent 路径、昵称、角色、深度及历史消息保留。历史正文中出现的旧 UUID 不作文本替换。
- 重映射历史事件、用量记录和 compaction 元数据中的结构化会话 ID，包括子 Agent 活动的 `agent_thread_id`。消息正文、历史工具输出和加密内容保持原值。
- 按原始创建日期生成 `sessions/YYYY/MM/DD/rollout-...-UUID.jsonl`；保留原有记录时间和 ordinal 顺序。
- 将 session metadata、turn context 中以原项目路径开头的结构化路径值映射到本机所选项目，包括工作区根目录和权限中的对应项目路径。其他项目、绝对工具路径和历史消息正文保持原值，需要单独配置。
- 移除源会话的 `creator_user_id` 和 `creator_account_id`，不冒用来源电脑的创建者身份。
- 保留分页 `history_base` 的 ordinal 边界，根据重新编码后的 UTF-8 长度逐层重算 `end_byte_offset`，并验证源偏移位于真实换行边界、对应的 ordinal 与偏移一致。
- 支持来源会话为 `.jsonl` 或 `.jsonl.zst`，从当前 `CODEX_HOME` 的 `sessions` 和 `archived_sessions` 查找依赖。

历史消息、工具输出、compaction 摘要和已有指令内容保留为原有 JSON 值。重写范围是机器元数据和结构化会话关联；历史正文中的来源路径仍能作为历史事实阅读。项目代码、外部附件、二进制、插件、MCP 服务、运行中的终端、工作树和云端 Page 不包含在迁移包中，不能通过复制 rollout 重建。新一轮能否使用原有工具，还取决于电脑 B 的环境与账户权限。

## Codex 注册和恢复

导入先在 CodexUI 本地数据目录的 `imports/<导入批次 UUID>/` 保存重建文件与 `receipt.json`，记录 UUID 和目标路径映射，然后使用 `create_new` 写入 Codex 会话目录。写入失败会撤销本批已经写入的文件。异常退出留下的批次可由收据检查，不会覆盖旧聊天。

接着启动本机 `codex app-server --listen stdio://`，在同一进程中按父会话在前的顺序处理整棵关系图。普通会话执行 `thread/resume`；子 Agent 执行 `thread/read` 来验证历史与关联，然后按需设置会话名并再次读取。不发起模型轮次。已完成或尚未加载的 v2 子 Agent 可能拒绝直接 `thread/resume`；迁移不会强制启动它们。历史分页不作为独立 Agent 注册。结果另存为该批次的 `registration.json`，只有所有会话均可读取时才报告完成。不直接编辑 Codex SQLite，不修改全局项目列表，不停止当前 Codex 实例。列表未刷新时可重新打开 Codex。

若本机没有兼容 CLI，或注册超时、登录与环境配置使恢复失败，文件和收据仍保留，界面会明确显示「文件已导入、注册未完成」，并给出 `codex resume <新 UUID>` 的恢复入口。不会把该状态报告为完整成功。

当前 CLI 的 `thread/read` 可能把未加载子 Agent 的运行态 `sessionId` 显示为子 Agent 自身的 UUID。迁移保留 rollout 中映射后的共享 `session_id`，并分别核对 `parentThreadId`、来源中的父 ID 与 fork 来源；验收报告会记录这类接口回退的数量。它不代表历史文件里的共享 session ID 被改成了子 Agent ID。

协议依据：[官方 App Server 文档](https://learn.chatgpt.com/docs/app-server)。`thread/resume.path` 是实验性字段；桌面菜单也是运行时 DOM 扩展，官方升级可能需要适配。当前实现结合本机生成的协议 schema 验证，隔离集成测试使用本机官方 CLI 实际注册分页会话并由第二个进程再次恢复。2026-10-23，用户另行手动完成了一次电脑 A 到另一台电脑的迁移，并确认成功。该反馈没有记录两台电脑的具体系统与版本，不能据此推断所有版本或 macOS 均已完成端到端验收。

## 验证

```powershell
pnpm install --frozen-lockfile
pnpm build
pnpm test
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo clippy --manifest-path src-tauri/Cargo.toml --lib --tests -- -D warnings
```

需要本机 CLI 的隔离集成测试单独运行：

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib codex_app_server_registers_import_in_isolated_home -- --ignored --nocapture
```

该测试使用合成历史、临时 `CODEX_HOME` 和临时项目目录，不修改真实会话，也不发起模型请求。普通测试覆盖多层依赖、中文 UTF-8 偏移、UUID 和项目映射、压缩来源、损坏包、路径穿越、循环依赖、导入预览竞态和快照历史保留。DOM 测试覆盖会话识别、原菜单保留、侧栏导入、键盘操作、页面初始注入和断线反馈。

真实 Agent 关系图验收使用显式指定的源目录，并只向新建的隔离目录写入。以下 PowerShell 命令不更改全局 `CODEX_HOME`，也不向会话发送消息：

```powershell
New-Item -ItemType Directory -Force .tmp/test-temp | Out-Null
$env:TEMP = (Resolve-Path .tmp/test-temp).Path
$env:TMP = $env:TEMP
$env:CODEXUI_VALIDATE_SOURCE_HOME = Join-Path $env:USERPROFILE '.codex'
$env:CODEXUI_VALIDATE_SESSION = '<源会话 UUID>'
$env:CODEXUI_VALIDATE_AGENTS = '14'
$env:CODEXUI_VALIDATE_OUTPUT = Join-Path $PWD '.tmp/agent-transfer-validation'
cargo test --manifest-path src-tauri/Cargo.toml --lib real_agent_graph_isolated_roundtrip -- --ignored --nocapture
```

测试逐条核对消息与工具输出、逐页检查记录数量和 UTF-8 分页边界，包括 fork 中内嵌的父会话元数据。核对映射后的父子关系、session ID 和 fork 来源，再用新的 app-server 进程重新读取整棵图。Codex 在恢复时可能向副本末尾追加 `thread_settings_applied` 事件；测试要求原有记录完整保留为转换后的前缀，并只允许该类设置事件出现在新增尾部。最后复查所有源文件 SHA-256，并写入 `report.json`。隔离目录包含完整私人历史，已由 `.tmp/` 规则排除在 Git 之外。运行中的模型任务、终端进程和工具连接不会随历史恢复。
