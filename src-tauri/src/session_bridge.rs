//! CDP bindings keep filesystem capabilities in Rust; no localhost HTTP API is exposed.
use serde_json::{json, Value};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tauri::Emitter;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tungstenite::{connect, stream::MaybeTlsStream, Message};
static STARTED: AtomicBool = AtomicBool::new(false);
const SCRIPT: &str = include_str!("session_menu.js");

#[tauri::command]
pub fn enable_session_menu(app: tauri::AppHandle) -> Result<(), String> {
    // Fail promptly when CDP is not ready, so the UI never claims installation succeeded.
    let pages = crate::targets()?;
    if !pages
        .iter()
        .any(|p| p["type"] == "page" && p["url"] == "app://-/index.html")
    {
        return Err("未找到 Codex 主窗口，请先连接 CDP".into());
    }
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        let active = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::BTreeSet::<String>::new(),
        ));
        loop {
            if let Ok(pages) = crate::targets() {
                for page in pages
                    .iter()
                    .filter(|p| p["type"] == "page" && p["url"] == "app://-/index.html")
                {
                    if let Some(ws) = page["webSocketDebuggerUrl"].as_str() {
                        if !active.lock().unwrap().insert(ws.to_string()) {
                            continue;
                        }
                        let active = active.clone();
                        let app = app.clone();
                        let ws = ws.to_string();
                        std::thread::spawn(move || {
                            let result = serve(&app, &ws);
                            active.lock().unwrap().remove(&ws);
                            let _ = app.emit(
                                "session-menu-status",
                                format!(
                                    "Codex 窗口连接已断开，正在重连… {}",
                                    result.err().unwrap_or_default()
                                ),
                            );
                        });
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
    Ok(())
}
fn serve(app: &tauri::AppHandle, ws: &str) -> Result<(), String> {
    let (mut socket, _) = connect(ws).map_err(|e| e.to_string())?;
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
    }
    let mut seq = 0u64;
    macro_rules! send { ($method:expr,$params:expr) => {{ seq+=1; socket.send(Message::Text(json!({"id":seq,"method":$method,"params":$params}).to_string().into())).map_err(|e|e.to_string())?; }}; }
    send!("Runtime.enable", json!({}));
    send!(
        "Runtime.addBinding",
        json!({"name":"__codexuiSessionAction"})
    );
    send!("Page.enable", json!({}));
    send!(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source":SCRIPT})
    );
    send!(
        "Runtime.evaluate",
        json!({"expression":format!("window.__codexuiSessionMenu?.dispose();{SCRIPT}")})
    );
    let injection_id = seq;
    let mut heartbeat = std::time::Instant::now();
    loop {
        if heartbeat.elapsed() >= Duration::from_secs(2) {
            send!(
                "Runtime.evaluate",
                json!({"expression":"window.__codexuiSessionHeartbeat=Date.now()"})
            );
            heartbeat = std::time::Instant::now();
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let Ok(event) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if event["id"] == injection_id {
                    if !event["error"].is_null() || !event["result"]["exceptionDetails"].is_null() {
                        return Err("无法安装会话菜单".into());
                    }
                    let _ = app.emit("session-menu-status", "会话右键菜单已连接");
                }
                if event["method"] == "Runtime.bindingCalled"
                    && event["params"]["name"] == "__codexuiSessionAction"
                {
                    let payload = event["params"]["payload"].as_str().unwrap_or("");
                    if payload.len() > 4096 {
                        continue;
                    }
                    let result = serde_json::from_str::<Value>(payload)
                        .map_err(|e| e.to_string())
                        .and_then(|request| handle(app, &request));
                    let message = match result {
                        Ok(s) => s,
                        Err(e) => format!("操作未完成：{e}"),
                    };
                    let _ = app.emit("session-transfer-result", &message);
                    send!(
                        "Runtime.evaluate",
                        json!({"expression":format!("window.__codexuiSessionMenu?.result({})",json!(message)),"contextId":event["params"]["executionContextId"]})
                    );
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => (),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}
fn folder(app: &tauri::AppHandle, title: &str) -> Result<Option<String>, String> {
    app.dialog()
        .file()
        .set_title(title)
        .blocking_pick_folder()
        .map(|p| {
            p.into_path()
                .map(|p| p.to_string_lossy().to_string())
                .map_err(|e| e.to_string())
        })
        .transpose()
}
fn handle(app: &tauri::AppHandle, request: &Value) -> Result<String, String> {
    match request["action"].as_str() {
        Some("sync") => {
            let id = request["id"]
                .as_str()
                .ok_or("未识别到本机会话 ID，请在 CodexUI 内选择会话")?;
            let Some(directory) = folder(app, "选择同步目录（可由你的同步软件同步到另一台电脑）")?
            else {
                return Ok("已取消同步设置".into());
            };
            let title = request["title"].as_str().unwrap_or(id);
            let path = crate::sessions::sync_session_impl(id.into(), directory, title.into())?;
            Ok(format!("已开始同步到：{path}\nCodexUI 运行期间每 15 秒更新一次。请在另一台电脑选择此迁移包目录导入。"))
        }
        Some("import") => {
            let Some(directory) = folder(app, "选择 codex-session-UUID 迁移包目录")? else {
                return Ok("已取消导入".into());
            };
            let preview = crate::sessions::preview_import_impl(directory.clone())?;
            let Some(project) = folder(
                app,
                &format!("选择本机项目目录（来源：{}）", preview.source_cwd),
            )?
            else {
                return Ok("已取消导入".into());
            };
            let confirmed=app.dialog().message(format!("会话：{}\n来源项目：{}\n本机项目：{}\n包含 {} 个历史文件。导入将创建独立会话，并保留完整历史中的指令与内容。是否导入？",preview.title,preview.source_cwd,project,preview.files)).title("导入会话").buttons(MessageDialogButtons::OkCancel).blocking_show();
            if !confirmed {
                return Ok("已取消导入".into());
            }
            let result =
                crate::sessions::import_session_impl(directory, project, preview.snapshot)?;
            Ok(format!("{}\n会话 UUID：{}", result.message, result.id))
        }
        _ => Err("不支持的会话操作".into()),
    }
}
