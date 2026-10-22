# 会话同步与导入

本功能针对本机 Codex rollout 会话，通过完整历史文件迁移上下文。数据存储保持本地，不依赖 Supabase 或云端服务。需要 CodexUI、可用的本机 Codex CLI，以及两台电脑之间已有的目录同步软件。云端聊天和没有本地 rollout 的会话不在此入口的支持范围内。

## 使用流程

电脑 A 在 CodexUI 中连接 Codex，并点击「启用 Codex 右键菜单」。右键侧栏中的会话，选择「同步到目录」，原生目录选择器会让你选择目标目录。原菜单里的选项会保留；当桌面客户端没有渲染可扩展菜单时，CodexUI 会显示独立菜单。CodexUI 自身的会话列表提供同样的同步入口，不依赖桌面客户端的 DOM 结构。

第一次同步成功后，CodexUI 会记住绑定，每 15 秒检查源会话。源文件没有变化时跳过重建，发生变化后才生成新快照。进程重新启动后自动恢复同步绑定；右键菜单在下一次连接 Codex 时自动恢复。多个 Codex 主窗口分别连接，不向透明辅助窗口注入。

由同步软件将整个 `codex-session-UUID` 目录同步到电脑 B。在电脑 B 的右键菜单或 CodexUI 内选择「导入会话」，选择该迁移包目录，再选择这台电脑上对应的项目目录，确认后导入。电脑 B 不需要原始项目路径与电脑 A 相同。

导入总是创建新会话，不覆盖已有聊天。重复导入同一快照也会得到另一个独立副本。两台电脑分别继续聊天后，本功能不会自动合并分叉内容。停止同步只移除本机绑定，保留目录中已有的迁移包。

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

导入验证清单 SHA-256、解压后大小、每个文件 SHA-256、JSON、会话 ID、完整依赖链与路径边界。同步软件尚未传完文件、内容损坏或出现循环依赖时停止导入，不创建会话。单个文件上限为解压后 512 MiB，总量上限 1 GiB，最多 256 个历史文件。

## UUID、日期和路径

- 每个导入的历史文件分配新 UUID，保留依赖关系。源会话文件保持原样。
- 按原始创建日期生成 `sessions/YYYY/MM/DD/rollout-...-UUID.jsonl`；保留原有记录时间和 ordinal 顺序。
- 将 session metadata、turn context 中以原项目路径开头的结构化路径值映射到本机所选项目，包括工作区根目录和权限中的对应项目路径。其他项目、绝对工具路径和历史消息正文保持原值，需要单独配置。
- 移除源会话的 `creator_user_id` 和 `creator_account_id`，不冒用来源电脑的创建者身份。
- 保留分页 `history_base` 的 ordinal 边界，根据重新编码后的 UTF-8 长度逐层重算 `end_byte_offset`，并验证源偏移位于真实换行边界、对应的 ordinal 与偏移一致。
- 支持来源会话为 `.jsonl` 或 `.jsonl.zst`，从当前 `CODEX_HOME` 的 `sessions` 和 `archived_sessions` 查找依赖。

历史消息、工具输出、compaction 和已有指令会保留为原有 JSON 值。只有机器元数据会重写；这意味着历史中提到的来源路径仍能作为历史事实阅读。项目代码、外部附件、二进制、插件、MCP 服务、运行中的终端、工作树和云端 Page 不包含在迁移包中，不能通过复制 rollout 重建。新一轮能否使用原有工具，还取决于电脑 B 的环境与账户权限。

## Codex 注册和恢复

导入先在 CodexUI 本地数据目录的 `imports/<导入批次 UUID>/` 保存重建文件与 `receipt.json`，记录 UUID 和目标路径映射，然后使用 `create_new` 写入 Codex 会话目录。写入失败会撤销本批已经写入的文件。异常退出留下的批次可由收据检查，不会覆盖旧聊天。

接着启动本机 `codex app-server --listen stdio://`，执行 `initialize`、`thread/resume` 和 `thread/name/set`，不发起模型轮次。不直接编辑 Codex SQLite，不修改全局项目列表，不停止当前 Codex 实例。列表未刷新时可重新打开 Codex。

若本机没有兼容 CLI，或注册超时、登录与环境配置使恢复失败，文件和收据仍保留，界面会明确显示「文件已导入、注册未完成」，并给出 `codex resume <新 UUID>` 的恢复入口。不会把该状态报告为完整成功。

协议依据：[官方 App Server 文档](https://learn.chatgpt.com/docs/app-server)。`thread/resume.path` 是实验性字段；桌面菜单也是运行时 DOM 扩展，官方升级可能需要适配。当前实现结合本机生成的协议 schema 验证，隔离集成测试使用本机官方 CLI 实际注册分页会话并由第二个进程再次恢复。没有宣称已完成两台实体电脑、所有版本或 macOS 的端到端验收。

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
