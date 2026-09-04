# CodexUI

![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-6f62d9)
![License](https://img.shields.io/badge/license-MIT-green)

CodexUI 是一款轻量、开源的 ChatGPT/Codex Desktop 壁纸管理器。它通过仅监听本机回环地址的 Chrome DevTools Protocol（CDP）向桌面客户端注入背景样式，不修改官方安装文件、应用签名或用户的原始图片。

> CodexUI 是社区项目，与 OpenAI 无隶属或背书关系。ChatGPT、Codex 和 OpenAI 是其各自所有者的商标。

## 效果展示

### 壁纸管理器

![CodexUI 壁纸管理界面](docs/images/codexui-app.png)

### 应用后的 Codex Desktop

![Codex Desktop 壁纸效果](docs/images/codex-wallpaper-demo.png)

## 功能

- 自动发现并启动官方 ChatGPT/Codex Desktop
- 通过本机 CDP 连接桌面客户端
- 导入、预览、切换、删除和取消壁纸
- 调节壁纸透明度、背景模糊和暗色遮罩
- 将注入严格限制在主窗口，不影响宠物等透明置顶辅助窗口
- 页面导航或重绘后自动恢复壁纸
- 所有图片均保存在本地

## 安装与使用

从仓库的 **Releases** 页面下载适合系统的安装包：

- Windows：NSIS `-setup.exe` 或 MSI
- Apple Silicon Mac：`aarch64` DMG
- Intel Mac：`x86_64` DMG

使用步骤：

1. 打开 CodexUI。
2. 如果 ChatGPT/Codex 已运行，先保存正在进行的工作，然后点击“重启并连接”。
3. 点击“添加壁纸”，选择 PNG、JPG、WebP、GIF 或 BMP 图片。
4. 调整显示参数，点击图片应用；点击“取消壁纸”恢复官方外观。

Windows 使用 `127.0.0.1:9335`。壁纸复制到 `%LOCALAPPDATA%\CodexUI\wallpapers`，原始图片不会被修改。

## 安全与隐私

- CDP 仅绑定 `127.0.0.1`，不会暴露到局域网。
- 开启 CDP 时，同一系统用户下的其他本地程序理论上可以访问调试端口；请勿同时运行不可信软件。
- CodexUI 不读取聊天内容、API Key、访问令牌或账户信息。
- 仓库和 Actions workflow 不包含证书、密码或签名私钥。

## 本地开发

需要 Node.js 22+、PNPM 10+、Rust stable，以及对应平台的 [Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
```

构建当前平台安装包：

```bash
pnpm tauri build
```

## 发布

推送形如 `v0.1.0` 的标签后，[GitHub Actions](.github/workflows/release.yml) 会自动创建 Release，并并行构建 Windows x64、macOS Apple Silicon 和 macOS Intel 安装包。应用版本应先在 `src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 和 `package.json` 中保持一致。

默认构建不需要自定义密钥，只使用 GitHub 自动提供的 `GITHUB_TOKEN`。面向最终用户正式分发时，建议配置代码签名，详见 [发布签名说明](docs/CODE_SIGNING.md)。

## 贡献

欢迎提交 Issue 和 Pull Request。提交前请至少运行：

```bash
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
```

## License

[MIT License](LICENSE)
