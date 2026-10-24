//! Portable, immutable session snapshots. Never edits Codex's SQLite or source rollouts.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};
use tauri::Emitter;
use uuid::Uuid;

static TRANSFER: Mutex<()> = Mutex::new(());
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_TOTAL: usize = 1024 * 1024 * 1024;
const MAX_FILES: usize = 256;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub timestamp: String,
    pub path: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: String,
    pub directory: String,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    file: String,
    sha256: String,
    bytes: usize,
    #[serde(default)]
    timestamp: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bundle {
    format: String,
    version: u32,
    root_id: String,
    title: String,
    cwd: String,
    entries: Vec<Entry>,
    /// Logical thread ID -> newest history page's storage ID (v2).
    #[serde(default)]
    threads: BTreeMap<String, String>,
}
type LoadedBundle = (Bundle, BTreeMap<String, Vec<u8>>, String);
type ThreadHeads = (BTreeMap<String, String>, BTreeMap<String, Value>);
struct Registration {
    id: String,
    path: String,
    cwd: String,
    title: String,
    resume: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub title: String,
    pub source_cwd: String,
    pub files: usize,
    pub threads: usize,
    pub agents: usize,
    pub bytes: usize,
    pub snapshot: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub id: String,
    pub path: String,
    pub registered: bool,
    pub message: String,
}

pub fn codex_home() -> Result<PathBuf, String> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".codex")))
        .ok_or("无法定位 CODEX_HOME".into())
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn uuid(s: &str) -> Result<(), String> {
    Uuid::parse_str(s)
        .map(|_| ())
        .map_err(|_| "无效的会话 UUID".into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let f = fs::File::open(path).map_err(err)?;
    if f.metadata().map_err(err)?.len() > MAX_FILE {
        return Err("会话文件超过 512 MiB 限制".into());
    }
    let mut out = Vec::new();
    if path.extension().is_some_and(|s| s == "zst") {
        zstd::stream::read::Decoder::new(f)
            .map_err(err)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut out)
            .map_err(err)?;
    } else {
        f.take(MAX_FILE + 1).read_to_end(&mut out).map_err(err)?;
    }
    if out.len() as u64 > MAX_FILE {
        return Err("解压后会话超过大小限制".into());
    }
    Ok(out)
}
fn records(bytes: &[u8]) -> Result<Vec<Value>, String> {
    if bytes.last() != Some(&b'\n') {
        return Err("会话仍在写入或文件不完整，请稍后重试".into());
    }
    bytes
        .split(|b| *b == b'\n')
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_slice(s).map_err(err))
        .collect()
}
fn metadata(bytes: &[u8]) -> Result<Value, String> {
    let line = bytes.split(|b| *b == b'\n').next().ok_or("空会话")?;
    let v: Value = serde_json::from_slice(line).map_err(err)?;
    if v["type"] != "session_meta" {
        return Err("缺少 session_meta".into());
    }
    uuid(v["payload"]["id"].as_str().ok_or("缺少会话 ID")?)?;
    Ok(v["payload"].clone())
}
fn read_metadata(path: &Path) -> Result<Value, String> {
    let f = fs::File::open(path).map_err(err)?;
    let reader: Box<dyn Read> = if path.extension().is_some_and(|s| s == "zst") {
        Box::new(zstd::stream::read::Decoder::new(f).map_err(err)?)
    } else {
        Box::new(f)
    };
    let mut line = String::new();
    BufReader::new(reader.take(2 * 1024 * 1024))
        .read_line(&mut line)
        .map_err(err)?;
    metadata(line.as_bytes())
}
fn agent_parent(m: &Value) -> Option<&str> {
    m["source"]["subagent"]["thread_spawn"]["parent_thread_id"].as_str()
}
fn thread_links(m: &Value) -> Vec<String> {
    [agent_parent(m), m["forked_from_id"].as_str()]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect()
}
/// Storage IDs identify history pages; session_meta.id identifies the logical thread.
fn thread_heads(index: &BTreeMap<String, PathBuf>) -> Result<ThreadHeads, String> {
    let mut heads: BTreeMap<String, String> = BTreeMap::new();
    let mut metas = BTreeMap::new();
    for (key, path) in index {
        let Ok(m) = read_metadata(path) else {
            continue;
        };
        let id = m["id"].as_str().ok_or("缺少会话 ID")?.to_string();
        if heads
            .get(&id)
            .is_none_or(|old| index[old].file_name() < path.file_name())
        {
            heads.insert(id, key.clone());
        }
        metas.insert(key.clone(), m);
    }
    Ok((heads, metas))
}
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    for item in fs::read_dir(dir).map_err(err)? {
        let item = item.map_err(err)?;
        let t = item.file_type().map_err(err)?;
        if t.is_symlink() {
            continue;
        }
        if t.is_dir() {
            walk(&item.path(), out)?;
        } else if item.file_name().to_string_lossy().ends_with(".jsonl")
            || item.file_name().to_string_lossy().ends_with(".jsonl.zst")
        {
            out.push(item.path());
        }
    }
    Ok(())
}
fn session_index(home: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut paths = vec![];
    walk(&home.join("archived_sessions"), &mut paths)?;
    walk(&home.join("sessions"), &mut paths)?;
    let mut out = BTreeMap::new();
    for p in paths {
        // Filenames let us index without reading message bodies or decompressing every session.
        let name = p.file_name().unwrap().to_string_lossy();
        let stem = name.trim_end_matches(".zst").trim_end_matches(".jsonl");
        if stem.len() >= 36 {
            if let Some(id) = stem.get(stem.len() - 36..) {
                if uuid(id).is_ok() {
                    out.insert(id.to_owned(), p.clone());
                }
            }
        }
    }
    Ok(out)
}
#[tauri::command]
pub async fn list_sessions() -> Result<Vec<Session>, String> {
    tauri::async_runtime::spawn_blocking(list_sessions_impl)
        .await
        .map_err(err)?
}
fn list_sessions_impl() -> Result<Vec<Session>, String> {
    let home = codex_home()?;
    let mut titles = BTreeMap::new();
    if let Ok(f) = fs::File::open(home.join("session_index.jsonl")) {
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let (Some(id), Some(title)) = (v["id"].as_str(), v["thread_name"].as_str()) {
                    titles.insert(id.to_string(), title.to_string());
                }
            }
        }
    }
    let mut out = vec![];
    let index = session_index(&home)?;
    let (heads, metas) = thread_heads(&index)?;
    for (id, head) in heads {
        let p = &index[&head];
        let m = &metas[&head];
        out.push(Session {
            title: titles.get(&id).cloned().unwrap_or_else(|| id.clone()),
            id,
            cwd: m["cwd"].as_str().unwrap_or("").into(),
            timestamp: m["timestamp"].as_str().unwrap_or("").into(),
            path: p.to_string_lossy().into(),
        });
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(out)
}
fn dependencies(m: &Value) -> Vec<String> {
    [m["history_base"]["thread_id"].as_str()]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect()
}
fn history_dependencies(values: &[Value]) -> BTreeSet<String> {
    values
        .iter()
        .filter(|v| v["type"] == "session_meta")
        .flat_map(|v| dependencies(&v["payload"]))
        .collect()
}
fn collect(
    id: &str,
    index: &BTreeMap<String, PathBuf>,
    visiting: &mut BTreeSet<String>,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    if out.iter().any(|(key, _)| key == id) {
        return Ok(());
    }
    uuid(id)?;
    if out.len() + visiting.len() >= MAX_FILES || !visiting.insert(id.into()) {
        return Err("会话依赖过多或存在循环引用".into());
    }
    let p = index
        .get(id)
        .ok_or_else(|| format!("缺少历史依赖 {id}，无法完整迁移"))?;
    let before = fs::metadata(p).map_err(err)?;
    let bytes = read_limited(p)?;
    let after = fs::metadata(p).map_err(err)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("会话正在更新，请待当前轮次结束后重试".into());
    }
    metadata(&bytes)?;
    let parents = history_dependencies(&records(&bytes)?);
    for parent in parents {
        collect(&parent, index, visiting, out)?;
    }
    if out.iter().map(|(_, b)| b.len()).sum::<usize>() + bytes.len() > MAX_TOTAL {
        return Err("迁移包超过 1 GiB 限制".into());
    }
    visiting.remove(id);
    out.push((id.into(), bytes));
    Ok(())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(err)?;
        f.write_all(bytes).map_err(err)?;
        f.sync_all().map_err(err)?;
        fs::rename(&tmp, path).map_err(err)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
fn export_to(home: &Path, id: &str, directory: &Path, title: &str) -> Result<String, String> {
    let index = session_index(home)?;
    let (heads, metas) = thread_heads(&index)?;
    let root_head = heads.get(id).ok_or("找不到源会话")?;
    let mut selected = BTreeSet::from([id.to_string()]);
    loop {
        let mut next = selected.clone();
        for thread in &selected {
            let head = heads
                .get(thread)
                .ok_or_else(|| format!("缺少 Agent 关联会话 {thread}"))?;
            next.extend(thread_links(&metas[head]));
            let mut page = head.as_str();
            let mut seen = BTreeSet::new();
            while seen.insert(page.to_string()) {
                let m = metas
                    .get(page)
                    .ok_or_else(|| format!("缺少历史依赖 {page}"))?;
                next.insert(m["id"].as_str().unwrap().to_string());
                next.extend(thread_links(m));
                match m["history_base"]["thread_id"].as_str() {
                    Some(base) => page = base,
                    None => break,
                }
            }
        }
        for (thread, head) in &heads {
            if agent_parent(&metas[head]).is_some_and(|p| selected.contains(p)) {
                next.insert(thread.clone());
            }
        }
        if next.len() > MAX_FILES {
            return Err("Agent 关系图过大".into());
        }
        if next == selected {
            break;
        }
        selected = next;
    }
    let mut files = vec![];
    let threads: BTreeMap<_, _> = selected
        .into_iter()
        .map(|thread| {
            let head = heads[&thread].clone();
            (thread, head)
        })
        .collect();
    let before = graph_fingerprint(&index, &threads)?;
    for head in threads.values() {
        collect(head, &index, &mut BTreeSet::new(), &mut files)?;
    }
    if before != graph_fingerprint(&session_index(home)?, &threads)? {
        return Err("会话关系图正在更新，请待当前轮次结束后重试".into());
    }
    let m = &metas[root_head];
    let bundle = Bundle {
        format: "codexui-session".into(),
        version: 2,
        root_id: id.into(),
        title: title.into(),
        cwd: m["cwd"].as_str().unwrap_or("").into(),
        entries: files
            .iter()
            .map(|(id, b)| Entry {
                id: id.clone(),
                file: format!("{id}.jsonl.zst"),
                sha256: digest(b),
                bytes: b.len(),
                timestamp: index[id]
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .get(8..27)
                    .unwrap_or("")
                    .into(),
            })
            .collect(),
        threads,
    };
    // Content-derived snapshots are immutable and safe for directory sync software.
    let manifest = serde_json::to_vec_pretty(&bundle).map_err(err)?;
    let snapshot = digest(&manifest);
    let package = directory.join(format!("codex-session-{id}"));
    fs::create_dir_all(package.join("snapshots")).map_err(err)?;
    if !package
        .join("snapshots")
        .canonicalize()
        .map_err(err)?
        .starts_with(directory.canonicalize().map_err(err)?)
    {
        return Err("同步目录中的快照路径越界".into());
    }
    let final_dir = package.join("snapshots").join(&snapshot);
    if !final_dir.exists() {
        let stage = package
            .join("snapshots")
            .join(format!(".{}", Uuid::new_v4()));
        fs::create_dir_all(&stage).map_err(err)?;
        let result = (|| {
            for (id, bytes) in &files {
                let compressed = zstd::stream::encode_all(bytes.as_slice(), 3).map_err(err)?;
                atomic_write(&stage.join(format!("{id}.jsonl.zst")), &compressed)?;
            }
            atomic_write(&stage.join("bundle.json"), &manifest)?;
            fs::rename(&stage, &final_dir).map_err(err)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result?;
    }
    if fs::read_to_string(package.join("CURRENT")).ok().as_deref() != Some(&snapshot) {
        atomic_write(&package.join("CURRENT"), snapshot.as_bytes())?;
    }
    Ok(package.to_string_lossy().into())
}
fn bindings_path() -> Result<PathBuf, String> {
    Ok(crate::base_dir()?.join("session-syncs.json"))
}
#[tauri::command]
pub fn list_syncs() -> Result<Vec<Binding>, String> {
    let p = bindings_path()?;
    if !p.exists() {
        return Ok(vec![]);
    }
    serde_json::from_slice(&fs::read(p).map_err(err)?).map_err(err)
}
#[tauri::command]
pub async fn sync_session(id: String, directory: String, title: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || sync_session_impl(id, directory, title))
        .await
        .map_err(err)?
}
pub fn sync_session_impl(id: String, directory: String, title: String) -> Result<String, String> {
    let _guard = TRANSFER.lock().map_err(err)?;
    let dir = PathBuf::from(&directory).canonicalize().map_err(err)?;
    if !dir.is_dir() {
        return Err("请选择同步目录".into());
    }
    let home = codex_home()?;
    if dir.starts_with(home.canonicalize().map_err(err)?) {
        return Err("同步目录不能位于 CODEX_HOME 内".into());
    }
    let package = export_to(&home, &id, &dir, &title)?;
    let mut items = list_syncs()?;
    items.retain(|b| b.id != id);
    items.push(Binding {
        id,
        directory: dir.to_string_lossy().into(),
        last_error: None,
    });
    atomic_write(
        &bindings_path()?,
        &serde_json::to_vec_pretty(&items).map_err(err)?,
    )?;
    Ok(package)
}
#[tauri::command]
pub async fn remove_sync(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || remove_sync_impl(id))
        .await
        .map_err(err)?
}
fn remove_sync_impl(id: String) -> Result<(), String> {
    let _guard = TRANSFER.lock().map_err(err)?;
    let mut items = list_syncs()?;
    items.retain(|b| b.id != id);
    atomic_write(
        &bindings_path()?,
        &serde_json::to_vec_pretty(&items).map_err(err)?,
    )
}
fn load_bundle(directory: &Path) -> Result<LoadedBundle, String> {
    let root = directory.canonicalize().map_err(err)?;
    if fs::metadata(root.join("CURRENT"))
        .map_err(|_| "请选择包含 CURRENT 的迁移包目录")?
        .len()
        != 64
    {
        return Err("无效的快照指针".into());
    }
    let current = fs::read_to_string(root.join("CURRENT"))
        .map_err(|_| "请选择 codex-session-UUID 迁移包目录（包含 CURRENT）")?;
    if current.len() != 64 || !current.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("无效的快照指针".into());
    }
    let snap = root
        .join("snapshots")
        .join(&current)
        .canonicalize()
        .map_err(|_| "快照尚未同步完整，请稍后重试")?;
    if !snap.starts_with(&root) {
        return Err("快照路径越界".into());
    }
    if fs::metadata(snap.join("bundle.json")).map_err(err)?.len() > 1024 * 1024 {
        return Err("迁移包清单过大".into());
    }
    let manifest = read_limited(&snap.join("bundle.json"))?;
    if digest(&manifest) != current {
        return Err("迁移包清单校验失败".into());
    }
    let mut bundle: Bundle = serde_json::from_slice(&manifest).map_err(err)?;
    if bundle.format != "codexui-session" || !(1..=2).contains(&bundle.version) {
        return Err("不支持的迁移包版本".into());
    }
    uuid(&bundle.root_id)?;
    if bundle.entries.is_empty() || bundle.entries.len() > MAX_FILES {
        return Err("迁移包文件数量无效".into());
    }
    let mut files = BTreeMap::new();
    let mut history = BTreeMap::new();
    let mut size = 0;
    for entry in &bundle.entries {
        uuid(&entry.id)?;
        if !entry.timestamp.is_empty()
            && (entry.timestamp.len() != 19
                || !entry.timestamp.bytes().enumerate().all(|(i, b)| match i {
                    4 | 7 | 13 | 16 => b == b'-',
                    10 => b == b'T',
                    _ => b.is_ascii_digit(),
                }))
        {
            return Err("无效历史页日期".into());
        }
        if (entry.file != format!("{}.jsonl", entry.id)
            && entry.file != format!("{}.jsonl.zst", entry.id))
            || files.contains_key(&entry.id)
        {
            return Err("迁移包文件路径或 ID 无效".into());
        }
        let p = snap
            .join(&entry.file)
            .canonicalize()
            .map_err(|_| "迁移包仍在同步，缺少会话文件")?;
        if !p.starts_with(&snap) {
            return Err("会话文件路径越界".into());
        }
        let bytes = read_limited(&p)?;
        size += bytes.len();
        if size > MAX_TOTAL || bytes.len() != entry.bytes || digest(&bytes) != entry.sha256 {
            return Err("迁移包不完整或内容校验失败，请等待同步结束".into());
        }
        if bundle.version == 1 && metadata(&bytes)?["id"] != entry.id {
            return Err("清单与会话 ID 不匹配".into());
        }
        metadata(&bytes)?;
        history.insert(entry.id.clone(), history_dependencies(&records(&bytes)?));
        files.insert(entry.id.clone(), bytes);
    }
    if bundle.version == 1 {
        bundle.threads = files.keys().map(|id| (id.clone(), id.clone())).collect();
    }
    if !bundle.threads.contains_key(&bundle.root_id) {
        return Err("迁移包缺少根会话".into());
    }
    for (thread, head) in &bundle.threads {
        uuid(thread)?;
        let m = metadata(files.get(head).ok_or("迁移包缺少会话末页")?)?;
        if m["id"] != *thread {
            return Err("会话末页 ID 不匹配".into());
        }
    }
    for (key, bytes) in &files {
        let m = metadata(bytes)?;
        if !bundle.threads.contains_key(m["id"].as_str().unwrap()) {
            return Err("历史页所属会话不在关系图中".into());
        }
        for dep in &history[key] {
            if !files.contains_key(dep) {
                return Err(format!("迁移包缺少依赖 {dep}"));
            }
        }
    }
    registration_order(&bundle, &files)?;
    Ok((bundle, files, current))
}
fn registration_order(
    bundle: &Bundle,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<String>, String> {
    fn visit(
        id: &str,
        bundle: &Bundle,
        files: &BTreeMap<String, Vec<u8>>,
        visiting: &mut BTreeSet<String>,
        done: &mut Vec<String>,
    ) -> Result<(), String> {
        if done.iter().any(|s| s == id) {
            return Ok(());
        }
        if !visiting.insert(id.into()) {
            return Err("Agent 关联存在循环".into());
        }
        let head = bundle
            .threads
            .get(id)
            .ok_or_else(|| format!("迁移包缺少 Agent 关联 {id}"))?;
        for dep in thread_links(&metadata(&files[head])?) {
            visit(&dep, bundle, files, visiting, done)?;
        }
        visiting.remove(id);
        done.push(id.into());
        Ok(())
    }
    let mut done = vec![];
    for id in bundle.threads.keys() {
        visit(id, bundle, files, &mut BTreeSet::new(), &mut done)?;
    }
    Ok(done)
}
#[tauri::command]
pub async fn preview_import(directory: String) -> Result<Preview, String> {
    tauri::async_runtime::spawn_blocking(move || preview_import_impl(directory))
        .await
        .map_err(err)?
}
pub fn preview_import_impl(directory: String) -> Result<Preview, String> {
    let (b, files, snapshot) = load_bundle(Path::new(&directory))?;
    let agents = b
        .threads
        .values()
        .filter(|head| metadata(&files[*head]).is_ok_and(|m| agent_parent(&m).is_some()))
        .count();
    Ok(Preview {
        title: b.title,
        source_cwd: b.cwd,
        files: files.len(),
        threads: b.threads.len(),
        agents,
        bytes: files.values().map(Vec::len).sum(),
        snapshot,
    })
}
fn remap(
    value: &mut Value,
    ids: &BTreeMap<String, String>,
    old_cwd: &str,
    cwd: &str,
    metadata_id: bool,
) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if key == "thread_id"
                    || key == "parent_thread_id"
                    || key == "forked_from_id"
                    || key == "session_id"
                    || (metadata_id && key == "id")
                {
                    if let Some(new) = v.as_str().and_then(|s| ids.get(s)) {
                        *v = Value::String(new.clone());
                    }
                } else {
                    remap(v, ids, old_cwd, cwd, false);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                remap(v, ids, old_cwd, cwd, false);
            }
        }
        Value::String(s)
            if !old_cwd.is_empty()
                && (s == old_cwd
                    || s.strip_prefix(old_cwd)
                        .is_some_and(|x| x.starts_with(['/', '\\']))) =>
        {
            let suffix = s[old_cwd.len()..].replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);
            *s = format!("{cwd}{suffix}");
        }
        _ => (),
    }
}
/// Update runtime links, without rewriting message text, encrypted content or historical tool output.
fn remap_history_ids(value: &mut Value, ids: &BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if matches!(
                    key.as_str(),
                    "thread_id"
                        | "threadId"
                        | "session_id"
                        | "sessionId"
                        | "agent_thread_id"
                        | "agentThreadId"
                        | "parent_thread_id"
                        | "parentThreadId"
                        | "sender_thread_id"
                        | "senderThreadId"
                        | "receiver_thread_id"
                        | "receiverThreadId"
                ) {
                    if let Some(new) = v.as_str().and_then(|id| ids.get(id)) {
                        *v = json!(new);
                    }
                } else if matches!(key.as_str(), "receiver_thread_ids" | "receiverThreadIds") {
                    if let Some(items) = v.as_array_mut() {
                        for item in items {
                            if let Some(new) = item.as_str().and_then(|id| ids.get(id)) {
                                *item = json!(new);
                            }
                        }
                    }
                } else {
                    remap_history_ids(v, ids);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                remap_history_ids(item, ids);
            }
        }
        _ => (),
    }
}
// Maps old newline boundaries to new UTF-8 byte offsets, including the metadata line.
fn rebuild(
    id: &str,
    files: &BTreeMap<String, Vec<u8>>,
    ids: &BTreeMap<String, String>,
    cwd: &str,
    visiting: &mut BTreeSet<String>,
    built: &mut BTreeMap<String, (Vec<u8>, BTreeMap<u64, u64>)>,
) -> Result<(), String> {
    if built.contains_key(id) {
        return Ok(());
    }
    if !visiting.insert(id.into()) {
        return Err("历史依赖循环".into());
    }
    let bytes = files.get(id).ok_or("历史依赖缺失")?;
    let m = metadata(bytes)?;
    let mut values = records(bytes)?;
    for dep in history_dependencies(&values) {
        rebuild(&dep, files, ids, cwd, visiting, built)?;
    }
    // Forks can embed additional parent metadata after their own header.
    // Every embedded history_base needs its own byte boundary adjustment.
    for value in values.iter_mut().filter(|v| v["type"] == "session_meta") {
        if let Some(base) = value["payload"].get_mut("history_base") {
            let parent = base["thread_id"].as_str().ok_or("无效 history_base")?;
            let offset = base["end_byte_offset"].as_u64().ok_or("缺少历史字节偏移")?;
            let new_offset = built
                .get(parent)
                .and_then(|(_, map)| map.get(&offset))
                .ok_or("历史字节偏移不在有效行边界，已停止导入")?;
            let source = files.get(parent).ok_or("缺少历史源文件")?;
            let prefix = source
                .get(..usize::try_from(offset).map_err(err)?)
                .ok_or("历史偏移超出源文件")?;
            let expected = base["end_ordinal_exclusive"]
                .as_u64()
                .ok_or("缺少历史 ordinal")?;
            let last = records(prefix)?
                .last()
                .and_then(|v| v["ordinal"].as_u64())
                .and_then(|n| n.checked_add(1));
            if last != Some(expected) {
                return Err("历史 ordinal 与字节偏移不匹配".into());
            }
            base["end_byte_offset"] = json!(new_offset);
        }
    }
    let old_cwd = m["cwd"].as_str().unwrap_or("");
    let mut out = vec![];
    let mut offsets = BTreeMap::from([(0, 0)]);
    let mut old_offset = 0;
    let lines: Vec<_> = bytes.split_inclusive(|b| *b == b'\n').collect();
    if lines.len() != values.len() {
        return Err("不支持含空行的会话历史".into());
    }
    for (i, v) in values.iter_mut().enumerate() {
        // Rewrite only machine metadata; user and assistant messages remain verbatim JSON values.
        if v["type"] == "session_meta" {
            remap(&mut v["payload"], ids, old_cwd, cwd, true);
            v["payload"]["cwd"] = json!(cwd);
            if let Some(m) = v["payload"].as_object_mut() {
                m.remove("creator_user_id");
                m.remove("creator_account_id");
            }
        } else if v["type"] == "turn_context" {
            remap(&mut v["payload"], ids, old_cwd, cwd, false);
        } else if v["type"] != "response_item" {
            remap_history_ids(&mut v["payload"], ids);
        }
        serde_json::to_writer(&mut out, v).map_err(err)?;
        out.push(b'\n');
        old_offset += lines[i].len() as u64;
        offsets.insert(old_offset, out.len() as u64);
    }
    visiting.remove(id);
    built.insert(id.into(), (out, offsets));
    Ok(())
}
fn date_path(meta: &Value) -> Result<PathBuf, String> {
    let stamp = meta["timestamp"].as_str().ok_or("会话缺少日期")?;
    let date = stamp.get(..10).ok_or("无效会话日期")?;
    let fields: Vec<_> = date.split('-').collect();
    if fields.len() != 3
        || fields[0].len() != 4
        || fields[1].len() != 2
        || fields[2].len() != 2
        || !fields.iter().all(|s| s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("无效会话日期".into());
    }
    let month: u8 = fields[1].parse().map_err(err)?;
    let day: u8 = fields[2].parse().map_err(err)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err("无效会话日期".into());
    }
    Ok(fields.iter().collect())
}
#[tauri::command]
pub async fn import_session(
    directory: String,
    project_directory: String,
    snapshot: String,
) -> Result<ImportResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        import_session_impl(directory, project_directory, snapshot)
    })
    .await
    .map_err(err)?
}
pub fn import_session_impl(
    directory: String,
    project_directory: String,
    snapshot: String,
) -> Result<ImportResult, String> {
    let _guard = TRANSFER.lock().map_err(err)?;
    import_into(
        directory,
        project_directory,
        snapshot,
        &codex_home()?,
        &crate::base_dir()?.join("imports"),
    )
}
fn import_into(
    directory: String,
    project_directory: String,
    snapshot: String,
    home: &Path,
    imports: &Path,
) -> Result<ImportResult, String> {
    let (bundle, files, current) = load_bundle(Path::new(&directory))?;
    if current != snapshot {
        return Err("迁移包已更新，请重新预览后导入".into());
    }
    let cwd = PathBuf::from(project_directory)
        .canonicalize()
        .map_err(err)?;
    if !cwd.is_dir() {
        return Err("本机项目目录不存在".into());
    }
    let cwd = display_path(&cwd);
    let ids: BTreeMap<_, _> = files
        .keys()
        .chain(bundle.threads.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| (id.clone(), Uuid::new_v4().to_string()))
        .collect();
    let mut built = BTreeMap::new();
    for key in files.keys() {
        rebuild(key, &files, &ids, &cwd, &mut BTreeSet::new(), &mut built)?;
    }
    // A journal is persisted before publishing. A crash leaves a reviewable list, never overwritten chats.
    let journal = imports.join(Uuid::new_v4().to_string());
    fs::create_dir_all(&journal).map_err(err)?;
    let mut destinations = BTreeMap::new();
    for (old, (bytes, _)) in &built {
        let m = metadata(bytes)?;
        let date = date_path(&m)?;
        let logical = metadata(&files[old])?["id"].as_str().unwrap().to_string();
        let suffix = if logical == *old {
            ids[old].clone()
        } else {
            format!("{}_{}", ids[&logical], ids[old])
        };
        let entry = bundle
            .entries
            .iter()
            .find(|e| e.id == *old)
            .ok_or("缺少历史页清单")?;
        let stamp = if entry.timestamp.is_empty() {
            m["timestamp"]
                .as_str()
                .unwrap()
                .get(..19)
                .ok_or("无效日期")?
                .replace(':', "-")
        } else {
            entry.timestamp.clone()
        };
        let filename = format!("rollout-{}-{}.jsonl", stamp, suffix);
        let dest = home.join("sessions").join(date).join(filename);
        if dest.exists() {
            return Err("新 UUID 意外冲突，已停止导入".into());
        }
        destinations.insert(old.clone(), dest);
        atomic_write(&journal.join(format!("{}.jsonl", ids[old])), bytes)?;
    }
    atomic_write(&journal.join("receipt.json"),&serde_json::to_vec_pretty(&json!({"source":directory,"snapshot":current,"ids":ids,"paths":destinations,"project":cwd,"threads":bundle.threads})).map_err(err)?)?;
    let mut published = vec![];
    let publish = (|| {
        for (old, dest) in &destinations {
            fs::create_dir_all(dest.parent().unwrap()).map_err(err)?;
            // create_new prevents any existing local conversation from being overwritten.
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(dest)
                .map_err(err)?;
            published.push(dest.clone());
            f.write_all(&built[old].0).map_err(err)?;
            f.sync_all().map_err(err)?;
        }
        Ok::<_, String>(())
    })();
    if let Err(e) = publish {
        for p in published {
            let _ = fs::remove_file(p);
        }
        return Err(e);
    }
    let id = ids[&bundle.root_id].clone();
    let path = destinations[&bundle.threads[&bundle.root_id]]
        .to_string_lossy()
        .to_string();
    let mut registrations = vec![];
    let registration = (|| {
        for thread in registration_order(&bundle, &files)? {
            let head = &bundle.threads[&thread];
            let meta = metadata(&files[head])?;
            let name = if thread == bundle.root_id {
                bundle.title.as_str()
            } else {
                meta["source"]["subagent"]["thread_spawn"]["agent_nickname"]
                    .as_str()
                    .unwrap_or("")
            };
            registrations.push(Registration {
                id: ids[&thread].clone(),
                path: display_path(&destinations[head]),
                cwd: cwd.clone(),
                title: name.to_string(),
                resume: agent_parent(&meta).is_none(),
            });
        }
        register_many(home, &registrations)?;
        Ok::<_, String>(())
    })();
    atomic_write(&journal.join("registration.json"), &serde_json::to_vec_pretty(&json!({
        "complete":registration.is_ok(),"threads":registrations.iter().map(|r| &r.id).collect::<Vec<_>>(),"total":bundle.threads.len(),"error":registration.as_ref().err()
    })).map_err(err)?)?;
    let (registered,message) = match registration {
        Ok(()) => (true,format!("导入完成，{} 个会话已通过 Codex app-server 注册。可在 Codex 会话列表中继续；列表未刷新时请重新打开 Codex。",bundle.threads.len())),
        Err(e) => (false,format!("会话文件已安全导入，但 Codex 注册未完成：{e}。可在终端运行 codex resume {id}；导入记录：{}",journal.display()))
    };
    Ok(ImportResult {
        id,
        path,
        registered,
        message,
    })
}
fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    }
}
fn codex_executable() -> String {
    if let Some(local) = dirs::data_local_dir() {
        let bin = local.join("OpenAI/Codex/bin");
        if let Ok(entries) = fs::read_dir(bin) {
            let mut candidates: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|e| e.path().join("codex.exe"))
                .filter(|p| p.is_file())
                .collect();
            candidates.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
            if let Some(p) = candidates.last() {
                return p.to_string_lossy().into();
            }
        }
    }
    for p in [
        "/Applications/Codex.app/Contents/Resources/codex",
        "/Applications/ChatGPT.app/Contents/Resources/codex",
        "/opt/homebrew/bin/codex",
        "/usr/local/bin/codex",
    ] {
        if Path::new(p).is_file() {
            return p.into();
        }
    }
    "codex".into()
}
#[cfg(test)]
fn register(home: &Path, id: &str, path: &str, cwd: &str, title: &str) -> Result<Value, String> {
    register_many(
        home,
        &[Registration {
            id: id.into(),
            path: path.into(),
            cwd: cwd.into(),
            title: title.into(),
            resume: true,
        }],
    )?
    .into_iter()
    .next()
    .ok_or("未返回会话".into())
}
fn register_many(home: &Path, registrations: &[Registration]) -> Result<Vec<Value>, String> {
    let mut child = crate::hidden_command(&codex_executable())
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法启动 codex：{e}"))?;
    let result = (|| {
        let mut input = child.stdin.take().ok_or("无法连接 Codex stdin")?;
        let output = child.stdout.take().ok_or("无法连接 Codex stdout")?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut request = |request_id: u64, method: &str, params: Value| -> Result<Value, String> {
            writeln!(
                input,
                "{}",
                json!({"id":request_id,"method":method,"params":params})
            )
            .map_err(err)?;
            input.flush().map_err(err)?;
            let deadline = std::time::Instant::now() + Duration::from_secs(45);
            loop {
                let line = rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .map_err(|_| "Codex 接口响应超时")?;
                let v: Value = serde_json::from_str(&line).map_err(err)?;
                if v["id"] == request_id {
                    if !v["error"].is_null() {
                        return Err(v["error"].to_string());
                    }
                    if method == "initialize" {
                        writeln!(input, "{}", json!({"method":"initialized","params":{}}))
                            .map_err(err)?;
                        input.flush().map_err(err)?;
                    }
                    return Ok(v["result"].clone());
                }
            }
        };
        request(
            1,
            "initialize",
            json!({"clientInfo":{"name":"codexui","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
        )?;
        let mut threads = vec![];
        for (i, registration) in registrations.iter().enumerate() {
            let Registration {
                id,
                path,
                cwd,
                title,
                resume,
            } = registration;
            let sequence = 2 + i as u64 * 3;
            let response = request(
                sequence,
                if *resume {
                    "thread/resume"
                } else {
                    "thread/read"
                },
                if *resume {
                    json!({"threadId":id,"path":path,"cwd":cwd,"excludeTurns":true})
                } else {
                    json!({"threadId":id,"includeTurns":false})
                },
            )
            .map_err(|e| format!("会话 {id} 注册失败：{e}"))?;
            if response["thread"]["id"] != *id {
                return Err("Codex 返回了不同的会话 ID".into());
            }
            if !title.is_empty() {
                request(
                    sequence + 1,
                    "thread/name/set",
                    json!({"threadId":id,"name":title}),
                )?;
            }
            let persisted = request(
                sequence + 2,
                "thread/read",
                json!({"threadId":id,"includeTurns":false}),
            )?;
            if persisted["thread"]["id"] != *id {
                return Err("Codex 未能读取已注册会话".into());
            }
            threads.push(persisted["thread"].clone());
        }
        Ok(threads)
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}
pub fn start_sync_worker(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut cache: BTreeMap<String, (Bundle, String)> = BTreeMap::new();
        loop {
            std::thread::sleep(Duration::from_secs(15));
            let Ok(_guard) = TRANSFER.try_lock() else {
                continue;
            };
            let Ok(mut items) = list_syncs() else {
                continue;
            };
            if items.is_empty() {
                continue;
            }
            let Ok(home) = codex_home() else {
                continue;
            };
            let Ok(index) = session_index(&home) else {
                continue;
            };
            cache.retain(|key, _| {
                items
                    .iter()
                    .any(|b| *key == format!("{}|{}", b.id, b.directory))
            });
            let mut changed = false;
            for binding in &mut items {
                let key = format!("{}|{}", binding.id, binding.directory);
                let package =
                    PathBuf::from(&binding.directory).join(format!("codex-session-{}", binding.id));
                if let Some((bundle, previous)) = cache.get(&key) {
                    if binding.last_error.is_none()
                        && Path::new(&binding.directory).is_dir()
                        && package.join("CURRENT").is_file()
                        && source_fingerprint(&index, bundle).ok().as_ref() == Some(previous)
                    {
                        continue;
                    }
                }
                let before = indexed_fingerprint(&index).ok();
                // Retain the original title; avoid opening Codex's state database.
                let title = cache
                    .get(&key)
                    .map(|(b, _)| b.title.clone())
                    .or_else(|| load_bundle(&package).ok().map(|(b, _, _)| b.title))
                    .unwrap_or_else(|| binding.id.clone());
                let error = if !Path::new(&binding.directory).is_dir() {
                    Some("同步目录不可用，将在下次自动重试".into())
                } else {
                    export_to(&home, &binding.id, Path::new(&binding.directory), &title).err()
                };
                if error != binding.last_error {
                    changed = true;
                    binding.last_error = error;
                }
                if binding.last_error.is_none() {
                    if let Ok((bundle, _, _)) = load_bundle(&package) {
                        if before.is_some() && before == indexed_fingerprint(&index).ok() {
                            if let Ok(fingerprint) = source_fingerprint(&index, &bundle) {
                                cache.insert(key, (bundle, fingerprint));
                            }
                        }
                    }
                }
            }
            if changed {
                if let (Ok(p), Ok(b)) = (bindings_path(), serde_json::to_vec_pretty(&items)) {
                    let _ = atomic_write(&p, &b);
                }
                let _ = app.emit("session-sync-updated", &items);
            }
        }
    });
}
fn indexed_fingerprint(index: &BTreeMap<String, PathBuf>) -> Result<String, String> {
    let mut hash = Sha256::new();
    for (id, path) in index {
        let m = fs::metadata(path).map_err(err)?;
        hash.update(id.as_bytes());
        hash.update(m.len().to_le_bytes());
        hash.update(
            m.modified()
                .map_err(err)?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(err)?
                .as_nanos()
                .to_le_bytes(),
        );
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn source_fingerprint(
    index: &BTreeMap<String, PathBuf>,
    bundle: &Bundle,
) -> Result<String, String> {
    let mut hash = Sha256::new();
    // Include new descendants without invalidating a snapshot for unrelated active chats.
    hash.update(graph_fingerprint(index, &bundle.threads)?.as_bytes());
    for entry in &bundle.entries {
        let m = fs::metadata(index.get(&entry.id).ok_or("会话源文件缺失")?).map_err(err)?;
        hash.update(entry.id.as_bytes());
        hash.update(m.len().to_le_bytes());
        hash.update(
            m.modified()
                .map_err(err)?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(err)?
                .as_nanos()
                .to_le_bytes(),
        );
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn graph_fingerprint(
    index: &BTreeMap<String, PathBuf>,
    threads: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut related = BTreeMap::new();
    for (key, path) in index {
        if let Ok(m) = read_metadata(path) {
            if m["id"].as_str().is_some_and(|id| threads.contains_key(id))
                || agent_parent(&m).is_some_and(|id| threads.contains_key(id))
            {
                if !threads.contains_key(m["id"].as_str().unwrap()) {
                    return Err("Agent 关系图出现新会话，请重新生成快照".into());
                }
                related.insert(key.clone(), path.clone());
            }
        }
    }
    indexed_fingerprint(&related)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "reads an explicitly selected real session; writes only to a fresh isolated validation directory"]
    fn real_agent_graph_isolated_roundtrip() {
        let source = PathBuf::from(
            std::env::var_os("CODEXUI_VALIDATE_SOURCE_HOME")
                .expect("set CODEXUI_VALIDATE_SOURCE_HOME"),
        );
        let root = std::env::var("CODEXUI_VALIDATE_SESSION").expect("set CODEXUI_VALIDATE_SESSION");
        let expected_agents: usize = std::env::var("CODEXUI_VALIDATE_AGENTS")
            .expect("set CODEXUI_VALIDATE_AGENTS")
            .parse()
            .unwrap();
        let base = PathBuf::from(
            std::env::var_os("CODEXUI_VALIDATE_OUTPUT").expect("set CODEXUI_VALIDATE_OUTPUT"),
        );
        fs::create_dir_all(&base).unwrap();
        let work = base
            .canonicalize()
            .unwrap()
            .join(Uuid::new_v4().to_string());
        assert!(!work.starts_with(source.canonicalize().unwrap()));
        let destination = work.join("codex-home");
        let project = work.join("project");
        let sync = work.join("export");
        let journals = work.join("imports");
        for p in [&destination, &project, &sync, &journals] {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(destination.join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
        println!("VALIDATION_DIR={}", work.display());
        let package = export_to(&source, &root, &sync, "Agent graph isolated validation").unwrap();
        let (bundle, files, snapshot) = load_bundle(Path::new(&package)).unwrap();
        let preview = preview_import_impl(package.clone()).unwrap();
        assert_eq!(preview.agents, expected_agents);
        assert_eq!(preview.threads, expected_agents + 1);
        let original_index = session_index(&source).unwrap();
        let source_hashes: BTreeMap<_, _> = files
            .iter()
            .map(|(key, bytes)| {
                assert!(
                    read_limited(&original_index[key]).unwrap() == *bytes,
                    "source snapshot changed: {key}"
                );
                (key.clone(), digest(bytes))
            })
            .collect();
        println!(
            "EXPORT_OK threads={} agents={} pages={} bytes={}",
            preview.threads, preview.agents, preview.files, preview.bytes
        );
        let result = import_into(
            package,
            display_path(&project),
            snapshot,
            &destination,
            &journals,
        )
        .unwrap();
        assert!(result.registered, "{}", result.message);
        let journal = fs::read_dir(&journals)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let receipt: Value =
            serde_json::from_slice(&fs::read(journal.join("receipt.json")).unwrap()).unwrap();
        let ids: BTreeMap<String, String> = serde_json::from_value(receipt["ids"].clone()).unwrap();
        let paths: BTreeMap<String, PathBuf> =
            serde_json::from_value(receipt["paths"].clone()).unwrap();
        let mut row_count = 0;
        let mut registration_records = 0;
        let mut relations = vec![];
        for (key, bytes) in &files {
            let original = records(bytes).unwrap();
            let imported_bytes = fs::read(&paths[key]).unwrap();
            let imported = records(&imported_bytes).unwrap();
            assert!(original.len() <= imported.len(), "history truncated: {key}");
            // Codex can append its own settings event when a thread is resumed.
            // All original rows must remain an exact transformed prefix.
            for added in &imported[original.len()..] {
                assert_eq!(
                    added["type"], "event_msg",
                    "unexpected appended record: {key}"
                );
                assert_eq!(
                    added["payload"]["type"], "thread_settings_applied",
                    "unexpected runtime activity: {key}"
                );
                let logical = metadata(bytes).unwrap()["id"].as_str().unwrap().to_string();
                assert_eq!(added["payload"]["thread_id"], ids[&logical]);
                registration_records += 1;
            }
            for (old, new) in original.iter().zip(&imported) {
                if old["type"] != "session_meta" && old["type"] != "turn_context" {
                    let mut expected = old.clone();
                    if old["type"] != "response_item" {
                        remap_history_ids(&mut expected["payload"], &ids);
                    }
                    assert!(
                        expected == *new,
                        "history changed: {key}, ordinal {}",
                        old["ordinal"]
                    );
                } else {
                    let mut expected = old.clone();
                    let old_cwd = metadata(bytes).unwrap()["cwd"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    let is_meta = old["type"] == "session_meta";
                    remap(
                        &mut expected["payload"],
                        &ids,
                        &old_cwd,
                        &display_path(&project),
                        is_meta,
                    );
                    if is_meta {
                        expected["payload"]["cwd"] = json!(display_path(&project));
                        expected["payload"]
                            .as_object_mut()
                            .unwrap()
                            .remove("creator_user_id");
                        expected["payload"]
                            .as_object_mut()
                            .unwrap()
                            .remove("creator_account_id");
                        // Offsets are independently checked against the imported page prefix below.
                        if expected["payload"].get("history_base").is_some() {
                            let base = old["payload"]["history_base"]["thread_id"]
                                .as_str()
                                .unwrap();
                            let offset = old["payload"]["history_base"]["end_byte_offset"]
                                .as_u64()
                                .unwrap() as usize;
                            let count = files[base][..offset]
                                .split_inclusive(|b| *b == b'\n')
                                .count();
                            let base_bytes = fs::read(&paths[base]).unwrap();
                            let new_offset: usize = base_bytes
                                .split_inclusive(|b| *b == b'\n')
                                .take(count)
                                .map(<[u8]>::len)
                                .sum();
                            expected["payload"]["history_base"]["end_byte_offset"] =
                                json!(new_offset);
                        }
                    }
                    assert!(
                        expected == *new,
                        "metadata mismatch: {key}, ordinal {}",
                        old["ordinal"]
                    );
                }
                row_count += 1;
            }
        }
        // A fresh app-server process must reopen every logical thread and report its identity/source.
        let order = registration_order(&bundle, &files).unwrap();
        let registrations: Vec<_> = order
            .iter()
            .map(|thread| Registration {
                id: ids[thread].clone(),
                path: display_path(&paths[&bundle.threads[thread]]),
                cwd: display_path(&project),
                title: String::new(),
                resume: agent_parent(&metadata(&files[&bundle.threads[thread]]).unwrap()).is_none(),
            })
            .collect();
        let reopened = register_many(&destination, &registrations).unwrap();
        let mut unloaded_session_id_fallbacks = 0;
        for (thread, info) in order.iter().zip(&reopened) {
            let head = &bundle.threads[thread];
            let original_meta = metadata(&files[head]).unwrap();
            if let Some(session) = original_meta["session_id"].as_str() {
                let persisted_meta = read_metadata(&paths[head]).unwrap();
                assert_eq!(
                    persisted_meta["session_id"], ids[session],
                    "persisted session tree: {thread}"
                );
                // Current app-server reports an unloaded child's own ID in this runtime field.
                // The persisted shared session ID and parent edges are checked separately.
                assert!(info["sessionId"] == ids[session] || info["sessionId"] == ids[thread]);
                if info["sessionId"] != ids[session] {
                    unloaded_session_id_fallbacks += 1;
                }
            }
            if let Some(fork) = original_meta["forked_from_id"].as_str() {
                assert_eq!(info["forkedFromId"], ids[fork], "persisted fork: {thread}");
            }
            if let Some(parent) = agent_parent(&original_meta) {
                assert_eq!(
                    info["parentThreadId"], ids[parent],
                    "persisted parent field: {thread}"
                );
                assert_eq!(
                    info["source"]["subAgent"]["thread_spawn"]["parent_thread_id"], ids[parent],
                    "persisted parent: {thread}, source={}",
                    info["source"]
                );
                relations.push(json!({"sourceId":thread,"importedId":ids[thread],"parent":ids[parent],"path":original_meta["source"]["subagent"]["thread_spawn"]["agent_path"]}));
            }
        }
        let imported_index = session_index(&destination).unwrap();
        let (imported_heads, _) = thread_heads(&imported_index).unwrap();
        for (thread, head) in &bundle.threads {
            assert_eq!(imported_heads[&ids[thread]], ids[head]);
        }
        for (key, hash) in &source_hashes {
            assert_eq!(
                digest(&read_limited(&original_index[key]).unwrap()),
                *hash,
                "source changed: {key}"
            );
        }
        let report = json!({"status":"PASS","sourceSession":root,"threads":preview.threads,"agents":preview.agents,"historyFiles":preview.files,"historyRecords":row_count,"registrationSettingsRecords":registration_records,"unloadedApiSessionIdFallbacks":unloaded_session_id_fallbacks,"bytes":preview.bytes,"sourceUnchanged":true,"allThreadsReopened":true,"modelTurnsStarted":0,"relations":relations,"sourceHashes":source_hashes,"importedRoot":result.id});
        fs::write(
            work.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!(
            "VALIDATION_PASS records={row_count} report={}",
            work.join("report.json").display()
        );
    }
    fn agent_fixture(home: &Path, id: &str, parent: &str) {
        let bytes = fixture(home, id, None);
        let mut rows = records(&bytes).unwrap();
        rows[0]["payload"]["source"] = json!({"subagent":{"thread_spawn":{
            "parent_thread_id":parent,"depth":2,"agent_path":"/root/a/b","agent_nickname":"Test"
        }}});
        rows[0]["payload"]["forked_from_id"] = json!(parent);
        rows[0]["payload"]["thread_source"] = json!("subagent");
        rows[0]["payload"]["multi_agent_version"] = json!("v2");
        let bytes: Vec<u8> = rows.into_iter().flat_map(line).collect();
        fs::write(&session_index(home).unwrap()[id], bytes).unwrap();
    }
    #[test]
    fn runtime_links_are_remapped_without_changing_history_text() {
        let old = Uuid::new_v4().to_string();
        let new = Uuid::new_v4().to_string();
        let ids = BTreeMap::from([(old.clone(), new.clone())]);
        let mut event = json!({"thread_id":old,"item":{"agent_thread_id":old,"senderThreadId":old,"receiverThreadIds":[old]},"text":old,"encrypted_content":old});
        remap_history_ids(&mut event, &ids);
        assert_eq!(event["thread_id"], new);
        assert_eq!(event["item"]["agent_thread_id"], new);
        assert_eq!(event["item"]["senderThreadId"], new);
        assert_eq!(event["item"]["receiverThreadIds"][0], new);
        assert_eq!(event["text"], old);
        assert_eq!(event["encrypted_content"], old);
    }
    #[test]
    fn legacy_v1_packages_remain_readable() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        let bytes = fixture(home.path(), &id, None);
        let package = PathBuf::from(export_to(home.path(), &id, sync.path(), "v1").unwrap());
        let (mut bundle, _, _) = load_bundle(&package).unwrap();
        bundle.version = 1;
        bundle.threads.clear();
        let mut manifest = serde_json::to_value(&bundle).unwrap();
        manifest.as_object_mut().unwrap().remove("threads");
        manifest["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("timestamp");
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let snapshot = digest(&manifest);
        let folder = package.join("snapshots").join(&snapshot);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("bundle.json"), manifest).unwrap();
        fs::write(
            folder.join(format!("{id}.jsonl.zst")),
            zstd::stream::encode_all(bytes.as_slice(), 3).unwrap(),
        )
        .unwrap();
        atomic_write(&package.join("CURRENT"), snapshot.as_bytes()).unwrap();
        let (bundle, files, _) = load_bundle(&package).unwrap();
        assert_eq!(bundle.threads, BTreeMap::from([(id.clone(), id.clone())]));
        assert_eq!(files[&id], bytes);
    }
    #[test]
    fn exports_descendants_and_shared_history_without_unrelated_sessions() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let root = Uuid::new_v4().to_string();
        let child = Uuid::new_v4().to_string();
        let grandchild = Uuid::new_v4().to_string();
        let unrelated = Uuid::new_v4().to_string();
        fixture(home.path(), &root, None);
        agent_fixture(home.path(), &child, &root);
        agent_fixture(home.path(), &grandchild, &child);
        fixture(home.path(), &unrelated, None);
        let package = export_to(home.path(), &root, sync.path(), "graph").unwrap();
        let (_, files, _) = load_bundle(Path::new(&package)).unwrap();
        assert_eq!(
            files.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([root, child, grandchild])
        );
        assert!(!files.contains_key(&unrelated));
    }
    #[test]
    fn paginated_agent_graph_remaps_logical_identity_and_retains_newest_page() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let root = Uuid::new_v4().to_string();
        let page = Uuid::new_v4().to_string();
        let child = Uuid::new_v4().to_string();
        let first = fixture(home.path(), &root, None);
        let last = fixture(home.path(), &page, Some((&root, first.len() as u64)));
        let mut rows = records(&last).unwrap();
        rows[0]["payload"]["id"] = json!(root);
        rows[0]["payload"]["session_id"] = json!(root);
        let old_path = session_index(home.path()).unwrap()[&page].clone();
        let new_path =
            old_path.with_file_name(format!("rollout-2026-10-24T12-00-00-{root}_{page}.jsonl"));
        fs::rename(old_path, &new_path).unwrap();
        fs::write(
            &new_path,
            rows.into_iter().flat_map(line).collect::<Vec<_>>(),
        )
        .unwrap();
        agent_fixture(home.path(), &child, &root);
        let inherited = records(&fs::read(&new_path).unwrap()).unwrap()[0].clone();
        OpenOptions::new()
            .append(true)
            .open(&session_index(home.path()).unwrap()[&child])
            .unwrap()
            .write_all(&line(inherited))
            .unwrap();
        let package = export_to(home.path(), &root, sync.path(), "pages").unwrap();
        let (bundle, files, _) = load_bundle(Path::new(&package)).unwrap();
        assert_eq!(bundle.threads[&root], page);
        assert_eq!(files.len(), 3);
        let ids: BTreeMap<_, _> = files
            .keys()
            .map(|id| (id.clone(), Uuid::new_v4().to_string()))
            .collect();
        let mut built = BTreeMap::new();
        for key in files.keys() {
            rebuild(
                key,
                &files,
                &ids,
                "/new/project",
                &mut BTreeSet::new(),
                &mut built,
            )
            .unwrap();
        }
        let last_meta = metadata(&built[&page].0).unwrap();
        let agent_meta = metadata(&built[&child].0).unwrap();
        assert_eq!(last_meta["id"], ids[&root]);
        assert_eq!(last_meta["session_id"], ids[&root]);
        assert_eq!(last_meta["history_base"]["thread_id"], ids[&root]);
        assert_eq!(
            last_meta["history_base"]["end_byte_offset"],
            built[&root].0.len()
        );
        assert_eq!(agent_parent(&agent_meta), Some(ids[&root].as_str()));
        assert_eq!(agent_meta["forked_from_id"], ids[&root]);
        assert_eq!(
            agent_meta["source"]["subagent"]["thread_spawn"]["agent_path"],
            "/root/a/b"
        );
        let inherited = records(&built[&child].0).unwrap()[2].clone();
        assert_eq!(inherited["payload"]["id"], ids[&root]);
        assert_eq!(
            inherited["payload"]["history_base"]["thread_id"],
            ids[&root]
        );
        assert_eq!(
            inherited["payload"]["history_base"]["end_byte_offset"],
            built[&root].0.len()
        );
        assert_eq!(
            records(&built[&page].0).unwrap()[1],
            records(&files[&page]).unwrap()[1]
        );
        let order = registration_order(&bundle, &files).unwrap();
        assert!(order.iter().position(|s| s == &root) < order.iter().position(|s| s == &child));
    }
    #[test]
    fn agent_changes_invalidate_sync_but_unrelated_chat_changes_do_not() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let root = Uuid::new_v4().to_string();
        let child = Uuid::new_v4().to_string();
        fixture(home.path(), &root, None);
        let package = export_to(home.path(), &root, sync.path(), "sync").unwrap();
        let (bundle, _, _) = load_bundle(Path::new(&package)).unwrap();
        let before = source_fingerprint(&session_index(home.path()).unwrap(), &bundle).unwrap();
        fixture(home.path(), &Uuid::new_v4().to_string(), None);
        assert_eq!(
            before,
            source_fingerprint(&session_index(home.path()).unwrap(), &bundle).unwrap()
        );
        agent_fixture(home.path(), &child, &root);
        assert!(source_fingerprint(&session_index(home.path()).unwrap(), &bundle).is_err());
    }
    #[test]
    fn refuses_missing_agent_parent_and_parent_cycles() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        agent_fixture(home.path(), &a, &b);
        assert!(export_to(home.path(), &a, sync.path(), "missing")
            .unwrap_err()
            .contains("缺少"));
        agent_fixture(home.path(), &b, &a);
        let package = export_to(home.path(), &a, sync.path(), "cycle").unwrap();
        assert!(load_bundle(Path::new(&package))
            .unwrap_err()
            .contains("循环"));
    }
    fn line(v: Value) -> Vec<u8> {
        let mut b = serde_json::to_vec(&v).unwrap();
        b.push(b'\n');
        b
    }
    #[test]
    fn compressed_source_exports_with_complete_content() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        let original = fixture(home.path(), &id, None);
        let p = session_index(home.path()).unwrap()[&id].clone();
        fs::write(
            p.with_extension("jsonl.zst"),
            zstd::stream::encode_all(original.as_slice(), 3).unwrap(),
        )
        .unwrap();
        fs::remove_file(p).unwrap();
        let package = export_to(home.path(), &id, sync.path(), "compressed").unwrap();
        let (_, files, _) = load_bundle(Path::new(&package)).unwrap();
        assert_eq!(files[&id], original);
    }
    #[test]
    fn refuses_manifest_path_traversal_and_dependency_cycles() {
        let home = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        fixture(home.path(), &id, None);
        let p = PathBuf::from(export_to(home.path(), &id, sync.path(), "x").unwrap());
        let (mut bundle, _, _) = load_bundle(&p).unwrap();
        bundle.entries[0].file = "../../outside.jsonl".into();
        let manifest = serde_json::to_vec_pretty(&bundle).unwrap();
        let snapshot = digest(&manifest);
        let dir = p.join("snapshots").join(&snapshot);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bundle.json"), manifest).unwrap();
        atomic_write(&p.join("CURRENT"), snapshot.as_bytes()).unwrap();
        assert!(load_bundle(&p).unwrap_err().contains("路径或 ID"));
        fixture(home.path(), &id, Some((&id, 12)));
        assert!(export_to(home.path(), &id, sync.path(), "x")
            .unwrap_err()
            .contains("循环"));
    }
    #[test]
    fn changed_snapshot_cannot_be_imported_after_preview() {
        let source = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let journals = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        fixture(source.path(), &id, None);
        let package = export_to(source.path(), &id, sync.path(), "before").unwrap();
        let preview = preview_import_impl(package.clone()).unwrap();
        export_to(source.path(), &id, sync.path(), "after").unwrap();
        assert!(import_into(
            package,
            project.path().to_string_lossy().into(),
            preview.snapshot,
            dest.path(),
            journals.path()
        )
        .err()
        .unwrap()
        .contains("已更新"));
        assert!(!dest.path().join("sessions").exists());
    }
    #[test]
    #[ignore = "requires installed Codex CLI; run explicitly with --ignored"]
    fn codex_app_server_registers_import_in_isolated_home() {
        let source = tempfile::tempdir().unwrap();
        let sync = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let journal = tempfile::tempdir().unwrap();
        fs::write(
            destination.path().join("config.toml"),
            "model = \"gpt-5.4\"\n",
        )
        .unwrap();
        let parent = Uuid::new_v4().to_string();
        let root = Uuid::new_v4().to_string();
        let p = fixture(source.path(), &parent, None);
        fixture(source.path(), &root, Some((&parent, p.len() as u64)));
        let package = export_to(source.path(), &root, sync.path(), "迁移注册验收").unwrap();
        let preview = preview_import_impl(package.clone()).unwrap();
        let result = import_into(
            package,
            project.path().to_string_lossy().into(),
            preview.snapshot,
            destination.path(),
            journal.path(),
        )
        .unwrap();
        assert!(result.registered, "{}", result.message);
        assert_ne!(result.id, root);
        assert!(Path::new(&result.path).is_file());
        assert_eq!(
            metadata(&fs::read(&result.path).unwrap()).unwrap()["cwd"],
            display_path(&project.path().canonicalize().unwrap())
        );
        // A second process must be able to reopen it, proving persistent registration.
        register(
            destination.path(),
            &result.id,
            &result.path,
            &display_path(project.path()),
            "迁移注册验收",
        )
        .unwrap();
        assert_eq!(session_index(destination.path()).unwrap().len(), 2);
    }
    fn fixture(home: &Path, id: &str, parent: Option<(&str, u64)>) -> Vec<u8> {
        let mut m = json!({"id":id,"session_id":id,"timestamp":"2026-10-23T08:10:12Z","cwd":"C:\\电脑A\\项目","history_mode":"paginated","originator":"codex_cli_rs","cli_version":"0.163.0","source":"cli","model_provider":"openai","runtime_workspace_roots":["C:\\电脑A\\项目"],"creator_user_id":"source-account"});
        let start = parent
            .and_then(|(id, _)| session_index(home).ok()?.get(id).cloned())
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| records(&b).ok())
            .and_then(|r| r.last().and_then(|v| v["ordinal"].as_u64()).map(|n| n + 1))
            .unwrap_or(if parent.is_some() { 2 } else { 0 });
        if let Some((p, offset)) = parent {
            m["history_base"] =
                json!({"thread_id":p,"end_ordinal_exclusive":start,"end_byte_offset":offset});
        }
        let mut b = line(
            json!({"timestamp":"2026-10-23T08:10:12Z","ordinal":start,"type":"session_meta","payload":m}),
        );
        b.extend(line(
            json!({"timestamp":"2026-10-23T08:10:12Z","ordinal":start+1,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"中文上下文 C:\\电脑A\\项目"}]}}),
        ));
        let dir = home.join("sessions/2026/10/23");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("rollout-2026-10-23T08-10-12-{id}.jsonl")),
            &b,
        )
        .unwrap();
        b
    }
    #[test]
    fn roundtrip_preserves_history_and_recalculates_utf8_lineage() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        let c = Uuid::new_v4().to_string();
        let original = fixture(home.path(), &a, None);
        let middle = fixture(home.path(), &b, Some((&a, original.len() as u64)));
        fixture(home.path(), &c, Some((&b, middle.len() as u64)));
        let package = export_to(home.path(), &c, target.path(), "完整会话").unwrap();
        let (_, files, _) = load_bundle(Path::new(&package)).unwrap();
        assert_eq!(files.len(), 3);
        let ids = files
            .keys()
            .map(|s| (s.clone(), Uuid::new_v4().to_string()))
            .collect();
        let mut built = BTreeMap::new();
        rebuild(
            &c,
            &files,
            &ids,
            "/Users/b/project",
            &mut BTreeSet::new(),
            &mut built,
        )
        .unwrap();
        let root = records(&built[&c].0).unwrap();
        let mid = metadata(&built[&b].0).unwrap();
        assert_eq!(root[0]["payload"]["history_base"]["thread_id"], ids[&b]);
        assert_eq!(
            root[0]["payload"]["history_base"]["end_byte_offset"],
            built[&b].0.len()
        );
        assert_eq!(mid["history_base"]["end_byte_offset"], built[&a].0.len());
        assert_eq!(root[0]["payload"]["cwd"], "/Users/b/project");
        assert_eq!(
            root[0]["payload"]["runtime_workspace_roots"][0],
            "/Users/b/project"
        );
        assert!(root[0]["payload"]["creator_user_id"].is_null());
        assert_eq!(root[1], records(&files[&c]).unwrap()[1]);
        assert_eq!(
            fs::read(session_index(home.path()).unwrap()[&a].clone()).unwrap(),
            original
        );
    }
    #[test]
    fn detects_tampered_or_incomplete_synced_files() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        fixture(home.path(), &id, None);
        let p = PathBuf::from(export_to(home.path(), &id, target.path(), "a").unwrap());
        let current = fs::read_to_string(p.join("CURRENT")).unwrap();
        fs::write(
            p.join("snapshots")
                .join(current)
                .join(format!("{id}.jsonl.zst")),
            zstd::stream::encode_all(&b"tampered"[..], 3).unwrap(),
        )
        .unwrap();
        assert!(load_bundle(&p).unwrap_err().contains("校验失败"));
    }
    #[test]
    fn refuses_missing_dependency_and_midline_offset() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4().to_string();
        let b = Uuid::new_v4().to_string();
        fixture(home.path(), &b, Some((&a, 3)));
        assert!(export_to(home.path(), &b, target.path(), "x").is_err());
        fixture(home.path(), &a, None);
        let p = export_to(home.path(), &b, target.path(), "x").unwrap();
        let (_, files, _) = load_bundle(Path::new(&p)).unwrap();
        let ids = files
            .keys()
            .map(|s| (s.clone(), Uuid::new_v4().to_string()))
            .collect();
        assert!(rebuild(
            &b,
            &files,
            &ids,
            "/b",
            &mut BTreeSet::new(),
            &mut BTreeMap::new()
        )
        .unwrap_err()
        .contains("行边界"));
    }
    #[test]
    fn immutable_snapshot_is_reused_and_previous_snapshot_survives() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4().to_string();
        fixture(home.path(), &id, None);
        let p = PathBuf::from(export_to(home.path(), &id, target.path(), "x").unwrap());
        let old = fs::read_to_string(p.join("CURRENT")).unwrap();
        export_to(home.path(), &id, target.path(), "x").unwrap();
        assert_eq!(fs::read_to_string(p.join("CURRENT")).unwrap(), old);
        let path = session_index(home.path()).unwrap()[&id].clone();
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(&line(json!({"type":"event_msg","payload":{"text":"new"}})))
            .unwrap();
        export_to(home.path(), &id, target.path(), "x").unwrap();
        assert_ne!(fs::read_to_string(p.join("CURRENT")).unwrap(), old);
        assert!(p.join("snapshots").join(old).exists());
    }
}
