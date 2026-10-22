use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};
use tungstenite::{connect, Message};
use uuid::Uuid;
mod session_bridge;
mod sessions;

const CDP_PORT: u16 = 9335;

fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wallpaper {
    id: String,
    name: String,
    path: String,
    data_url: String,
    added_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CdpStatus {
    running: bool,
    connected: bool,
    port: u16,
    message: String,
    executable: Option<String>,
}

fn base_dir() -> Result<PathBuf, String> {
    let dir = dirs::data_local_dir()
        .ok_or("无法定位本地数据目录")?
        .join("CodexUI");
    fs::create_dir_all(dir.join("wallpapers")).map_err(|e| e.to_string())?;
    Ok(dir)
}
fn manifest_path() -> Result<PathBuf, String> {
    Ok(base_dir()?.join("wallpapers.json"))
}
fn load_items() -> Result<Vec<Wallpaper>, String> {
    let p = manifest_path()?;
    if !p.exists() {
        return Ok(vec![]);
    };
    serde_json::from_str(&fs::read_to_string(p).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn save_items(v: &[Wallpaper]) -> Result<(), String> {
    fs::write(
        manifest_path()?,
        serde_json::to_string_pretty(v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => "image/jpeg",
    }
}
fn as_data_url(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    Ok(format!(
        "data:{};base64,{}",
        mime(path),
        STANDARD.encode(bytes)
    ))
}

#[cfg(target_os = "windows")]
fn find_chatgpt() -> Option<String> {
    let cache = base_dir().ok()?.join("chatgpt-path.txt");
    let script="Get-Process ChatGPT -ErrorAction SilentlyContinue | Where-Object Path | Select-Object -First 1 -ExpandProperty Path";
    let out = hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .output()
        .ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !p.is_empty() {
        let _ = fs::write(&cache, &p);
        return Some(p);
    }
    let script2="Get-AppxPackage OpenAI.ChatGPT,OpenAI.Codex -ErrorAction SilentlyContinue | ForEach-Object { Get-ChildItem $_.InstallLocation -Filter ChatGPT.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName } | Select-Object -First 1";
    let out = hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script2,
        ])
        .output()
        .ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !p.is_empty() {
        let _ = fs::write(&cache, &p);
        return Some(p);
    }
    fs::read_to_string(cache)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| Path::new(s).is_file())
}

#[cfg(target_os = "macos")]
fn find_chatgpt() -> Option<String> {
    [
        "/Applications/Codex.app/Contents/MacOS/ChatGPT",
        "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
    ]
    .into_iter()
    .find(|p| Path::new(p).is_file())
    .map(str::to_string)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn find_chatgpt() -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
fn is_running() -> bool {
    hidden_command("tasklist.exe")
        .args(["/FI", "IMAGENAME eq ChatGPT.exe", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("ChatGPT.exe"))
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn is_running() -> bool {
    hidden_command("pgrep")
        .args(["-x", "ChatGPT"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn is_running() -> bool {
    false
}
fn targets() -> Result<Vec<Value>, String> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?
        .get(format!("http://127.0.0.1:{CDP_PORT}/json"))
        .send()
        .map_err(|_| "CDP 未响应".to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
fn package_aumid(exe: &str) -> Result<String, String> {
    let app_dir = Path::new(exe).parent().ok_or("无效的 ChatGPT 路径")?;
    let package_root = app_dir.parent().ok_or("无法定位 ChatGPT 应用包")?;
    let folder = package_root
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("无效的应用包名称")?;
    let parts: Vec<&str> = folder.split('_').collect();
    if parts.len() < 5 || parts[0] != "OpenAI.Codex" {
        return Err("当前 ChatGPT 不是受支持的官方 OpenAI.Codex Store 包".into());
    }
    Ok(format!("{}_{}!App", parts[0], parts[parts.len() - 1]))
}

#[cfg(target_os = "windows")]
fn activate_package_with_cdp(exe: &str) -> Result<(), String> {
    let aumid = package_aumid(exe)?;
    let arguments =
        format!("--remote-debugging-address=127.0.0.1 --remote-debugging-port={CDP_PORT}");
    let ps = format!(
        r#"Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IApplicationActivationManager {{ int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId, [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId); }}
[ComImport, Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c")] class ApplicationActivationManager {{}}
public static class CodexUILauncher {{ public static uint Launch(string id,string args) {{ var m=(IApplicationActivationManager)new ApplicationActivationManager(); uint pid; int hr=m.ActivateApplication(id,args,0,out pid); Marshal.ThrowExceptionForHR(hr); return pid; }} }}
'@
[CodexUILauncher]::Launch('{}','{}')"#,
        aumid, arguments
    );
    let encoded = STANDARD.encode(
        ps.encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let mut command = hidden_command("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-EncodedCommand",
        &encoded,
    ]);
    let out = command
        .output()
        .map_err(|e| format!("应用包激活失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Windows 无法激活 ChatGPT 应用包：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    for _ in 0..80 {
        thread::sleep(Duration::from_millis(250));
        if targets().is_ok() {
            return Ok(());
        }
    }
    Err("ChatGPT 应用包已启动，但 CDP 端点未建立。请检查是否安装了 Microsoft Store 正式版 OpenAI.Codex。".into())
}

#[cfg(target_os = "macos")]
fn activate_package_with_cdp(exe: &str) -> Result<(), String> {
    hidden_command(exe)
        .args([
            format!("--remote-debugging-address=127.0.0.1"),
            format!("--remote-debugging-port={CDP_PORT}"),
        ])
        .spawn()
        .map_err(|e| format!("启动失败：{e}"))?;
    for _ in 0..80 {
        thread::sleep(Duration::from_millis(250));
        if targets().is_ok() {
            return Ok(());
        }
    }
    Err("ChatGPT 已启动，但 CDP 端点未建立。请确认安装了官方 Codex/ChatGPT.app。".into())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn activate_package_with_cdp(_exe: &str) -> Result<(), String> {
    Err("当前系统暂不受支持".into())
}

#[cfg(target_os = "windows")]
fn terminate_chatgpt() -> Result<(), String> {
    let result = hidden_command("taskkill.exe")
        .args(["/F", "/T", "/IM", "ChatGPT.exe"])
        .output()
        .map_err(|e| format!("无法关闭 ChatGPT：{e}"))?;
    if !result.status.success() && is_running() {
        Err("无法关闭现有 ChatGPT，请从系统托盘退出后重试".into())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn terminate_chatgpt() -> Result<(), String> {
    let _ = hidden_command("osascript")
        .args(["-e", "tell application \"ChatGPT\" to quit"])
        .status();
    let _ = hidden_command("osascript")
        .args(["-e", "tell application \"Codex\" to quit"])
        .status();
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn terminate_chatgpt() -> Result<(), String> {
    Err("当前系统暂不受支持".into())
}

#[tauri::command]
fn cdp_status() -> CdpStatus {
    let running = is_running();
    let connected = targets()
        .map(|t| t.iter().any(|x| x["type"] == "page"))
        .unwrap_or(false);
    let executable = find_chatgpt();
    let message = if connected {
        "已连接 ChatGPT，可以切换壁纸"
    } else if running {
        "现有 ChatGPT 未开启 CDP；请完全退出后点击一键启动"
    } else if executable.is_some() {
        "已找到 ChatGPT Desktop，可以一键启动"
    } else {
        "未找到 ChatGPT Desktop 安装"
    }
    .to_string();
    CdpStatus {
        running,
        connected,
        port: CDP_PORT,
        message,
        executable,
    }
}

#[tauri::command]
fn launch_chatgpt() -> Result<CdpStatus, String> {
    if targets().is_ok() {
        return Ok(cdp_status());
    }
    if is_running() {
        return Err(
            "ChatGPT 正在运行但未开启 CDP。请从系统托盘完全退出 ChatGPT，然后再点击“一键启动”。"
                .into(),
        );
    }
    let exe = find_chatgpt().ok_or("未找到 ChatGPT Desktop，请先安装或启动一次应用")?;
    activate_package_with_cdp(&exe)?;
    Ok(cdp_status())
}

#[tauri::command]
fn restart_chatgpt_with_cdp() -> Result<CdpStatus, String> {
    if targets().is_ok() {
        return Ok(cdp_status());
    }
    let exe = find_chatgpt().ok_or("未找到 ChatGPT Desktop 的安装路径")?;
    terminate_chatgpt()?;
    for _ in 0..40 {
        if !is_running() {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    if is_running() {
        return Err("仍有 ChatGPT 进程未退出，请在任务管理器中结束后重试".into());
    }
    thread::sleep(Duration::from_millis(800));
    activate_package_with_cdp(&exe)?;
    Ok(cdp_status())
}

#[tauri::command]
fn list_wallpapers() -> Result<Vec<Wallpaper>, String> {
    load_items()
}

#[tauri::command]
fn add_wallpapers(paths: Vec<String>) -> Result<Vec<Wallpaper>, String> {
    let mut items = load_items()?;
    let dir = base_dir()?.join("wallpapers");
    for raw in paths {
        let src = PathBuf::from(raw);
        if !src.is_file() {
            continue;
        }
        let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("jpg");
        let id = Uuid::new_v4().to_string();
        let dest = dir.join(format!("{id}.{ext}"));
        fs::copy(&src, &dest).map_err(|e| e.to_string())?;
        items.push(Wallpaper {
            id,
            name: src
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("未命名壁纸")
                .to_string(),
            path: dest.to_string_lossy().to_string(),
            data_url: as_data_url(&dest)?,
            added_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
    }
    save_items(&items)?;
    Ok(items)
}

#[tauri::command]
fn remove_wallpaper(id: String) -> Result<Vec<Wallpaper>, String> {
    let mut items = load_items()?;
    if let Some(w) = items.iter().find(|w| w.id == id) {
        let _ = fs::remove_file(&w.path);
    }
    items.retain(|w| w.id != id);
    save_items(&items)?;
    Ok(items)
}

fn clear_expression() -> &'static str {
    r#"(()=>{for(const id of ['codexui-theme','wallshift-theme'])document.getElementById(id)?.remove();window.__codexuiObserver?.disconnect();window.__wallshiftObserver?.disconnect();delete window.__codexuiObserver;delete window.__codexuiCss;delete window.__wallshiftObserver;delete window.__wallshiftCss;return 'cleaned'})()"#
}

fn eval_target(ws: &str, expression: &str) -> bool {
    if let Ok((mut socket, _)) = connect(ws) {
        let msg = json!({"id":1,"method":"Runtime.evaluate","params":{"expression":expression,"awaitPromise":false}});
        // Delivery is enough here. Some auxiliary targets never return an evaluate result,
        // so waiting synchronously would block the Tauri command and freeze the UI.
        let sent = socket.send(Message::Text(msg.to_string().into())).is_ok();
        let _ = socket.close(None);
        return sent;
    }
    false
}

#[tauri::command]
fn clear_wallpaper() -> Result<(), String> {
    let pages = targets()?;
    let mut count = 0;
    for p in pages.iter().filter(|p| p["type"] == "page") {
        if let Some(ws) = p["webSocketDebuggerUrl"].as_str() {
            if eval_target(ws, clear_expression()) {
                count += 1
            }
        }
    }
    if count == 0 {
        Err("没有找到可清理的 ChatGPT 页面".into())
    } else {
        Ok(())
    }
}

#[tauri::command]
fn apply_wallpaper(id: String, opacity: f64, blur: u32, dim: f64) -> Result<(), String> {
    let pages = targets()?;
    // Remove old injections from every renderer, including auxiliary always-on-top windows.
    for p in pages.iter().filter(|p| p["type"] == "page") {
        if let Some(ws) = p["webSocketDebuggerUrl"].as_str() {
            let _ = eval_target(ws, clear_expression());
        }
    }
    let item = load_items()?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or("壁纸不存在")?;
    let data = as_data_url(Path::new(&item.path))?;
    let css = format!(
        r#"
html::before{{content:'';position:fixed;inset:-{blur}px;z-index:-2;background-image:url('{}');background-size:cover;background-position:center;filter:blur({blur}px);opacity:{opacity};pointer-events:none}}
html::after{{content:'';position:fixed;inset:0;z-index:-1;background:rgba(0,0,0,{dim});pointer-events:none}}
html,body,#root{{background:transparent!important}} body>div{{background-color:transparent!important}}
main,[class*='bg-token-main-surface'],[class*='bg-token-sidebar-surface']{{background-color:rgba(20,20,24,.58)!important;backdrop-filter:blur(12px)}}
"#,
        data.replace('\'', "%27")
    );
    let js = format!(
        r#"(()=>{{const id='codexui-theme';let s=document.getElementById(id);if(!s){{s=document.createElement('style');s.id=id;document.documentElement.appendChild(s)}}s.textContent={};window.__codexuiCss=s.textContent;if(!window.__codexuiObserver){{window.__codexuiObserver=new MutationObserver(()=>{{if(!document.getElementById(id)){{const n=document.createElement('style');n.id=id;n.textContent=window.__codexuiCss;document.documentElement.appendChild(n)}}}});window.__codexuiObserver.observe(document.documentElement,{{childList:true,subtree:true}})}}return 'ok'}})()"#,
        serde_json::to_string(&css).unwrap()
    );
    let mut count = 0;
    // Only the exact root URL is the main shell. Query routes include the transparent pet overlay.
    for p in pages
        .iter()
        .filter(|p| p["type"] == "page" && p["url"] == "app://-/index.html")
    {
        if let Some(ws) = p["webSocketDebuggerUrl"].as_str() {
            if eval_target(ws, &js) {
                count += 1
            }
        }
    }
    if count == 0 {
        Err("没有找到可注入的 ChatGPT 页面，请确认 CDP 已连接".into())
    } else {
        Ok(())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            cdp_status,
            launch_chatgpt,
            restart_chatgpt_with_cdp,
            list_wallpapers,
            add_wallpapers,
            remove_wallpaper,
            apply_wallpaper,
            clear_wallpaper,
            sessions::list_sessions,
            sessions::sync_session,
            sessions::preview_import,
            sessions::import_session,
            sessions::list_syncs,
            sessions::remove_sync,
            session_bridge::enable_session_menu
        ])
        .setup(|app| {
            sessions::start_sync_worker(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running CodexUI");
}
