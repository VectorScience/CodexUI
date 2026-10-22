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
}
type LoadedBundle = (Bundle, BTreeMap<String, Vec<u8>>, String);
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub title: String,
    pub source_cwd: String,
    pub files: usize,
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
    for (id, p) in session_index(&home)? {
        let meta = if p.extension().is_some_and(|s| s == "zst") {
            read_limited(&p).and_then(|b| metadata(&b))
        } else {
            fs::File::open(&p).map_err(err).and_then(|f| {
                let mut line = String::new();
                BufReader::new(f)
                    .take(2 * 1024 * 1024)
                    .read_line(&mut line)
                    .map_err(err)?;
                metadata(line.as_bytes())
            })
        };
        if let Ok(m) = meta {
            out.push(Session {
                title: titles.get(&id).cloned().unwrap_or_else(|| id.clone()),
                id,
                cwd: m["cwd"].as_str().unwrap_or("").into(),
                timestamp: m["timestamp"].as_str().unwrap_or("").into(),
                path: p.to_string_lossy().into(),
            });
        }
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(out)
}
fn dependencies(m: &Value) -> Vec<String> {
    [
        m["history_base"]["thread_id"].as_str(),
        m["forked_from_id"].as_str(),
    ]
    .into_iter()
    .flatten()
    .map(str::to_owned)
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
    let m = metadata(&bytes)?;
    if m["id"] != id {
        return Err("会话文件与 UUID 不匹配".into());
    }
    records(&bytes)?;
    for parent in dependencies(&m) {
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
    let mut files = vec![];
    collect(id, &index, &mut BTreeSet::new(), &mut files)?;
    let m = metadata(&files.last().ok_or("空迁移包")?.1)?;
    let bundle = Bundle {
        format: "codexui-session".into(),
        version: 1,
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
            })
            .collect(),
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
    let bundle: Bundle = serde_json::from_slice(&manifest).map_err(err)?;
    if bundle.format != "codexui-session" || bundle.version != 1 {
        return Err("不支持的迁移包版本".into());
    }
    uuid(&bundle.root_id)?;
    if bundle.entries.is_empty() || bundle.entries.len() > MAX_FILES {
        return Err("迁移包文件数量无效".into());
    }
    let mut files = BTreeMap::new();
    let mut size = 0;
    for entry in &bundle.entries {
        uuid(&entry.id)?;
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
        if metadata(&bytes)?["id"] != entry.id {
            return Err("清单与会话 ID 不匹配".into());
        }
        records(&bytes)?;
        files.insert(entry.id.clone(), bytes);
    }
    if !files.contains_key(&bundle.root_id) {
        return Err("迁移包缺少根会话".into());
    }
    for bytes in files.values() {
        for dep in dependencies(&metadata(bytes)?) {
            if !files.contains_key(&dep) {
                return Err(format!("迁移包缺少依赖 {dep}"));
            }
        }
    }
    Ok((bundle, files, current))
}
#[tauri::command]
pub async fn preview_import(directory: String) -> Result<Preview, String> {
    tauri::async_runtime::spawn_blocking(move || preview_import_impl(directory))
        .await
        .map_err(err)?
}
pub fn preview_import_impl(directory: String) -> Result<Preview, String> {
    let (b, files, snapshot) = load_bundle(Path::new(&directory))?;
    Ok(Preview {
        title: b.title,
        source_cwd: b.cwd,
        files: files.len(),
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
    for dep in dependencies(&m) {
        rebuild(&dep, files, ids, cwd, visiting, built)?;
    }
    let mut values = records(bytes)?;
    if let Some(base) = values[0]["payload"].get_mut("history_base") {
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
        .map(|id| (id.clone(), Uuid::new_v4().to_string()))
        .collect();
    let mut built = BTreeMap::new();
    rebuild(
        &bundle.root_id,
        &files,
        &ids,
        &cwd,
        &mut BTreeSet::new(),
        &mut built,
    )?;
    // A journal is persisted before publishing. A crash leaves a reviewable list, never overwritten chats.
    let journal = imports.join(Uuid::new_v4().to_string());
    fs::create_dir_all(&journal).map_err(err)?;
    let mut destinations = BTreeMap::new();
    for (old, (bytes, _)) in &built {
        let m = metadata(bytes)?;
        let date = date_path(&m)?;
        let filename = format!(
            "rollout-{}T00-00-00-{}.jsonl",
            m["timestamp"].as_str().unwrap().get(..10).unwrap(),
            ids[old]
        );
        let dest = home.join("sessions").join(date).join(filename);
        if dest.exists() {
            return Err("新 UUID 意外冲突，已停止导入".into());
        }
        destinations.insert(old.clone(), dest);
        atomic_write(&journal.join(format!("{}.jsonl", ids[old])), bytes)?;
    }
    atomic_write(&journal.join("receipt.json"),&serde_json::to_vec_pretty(&json!({"source":directory,"snapshot":current,"ids":ids,"paths":destinations,"project":cwd})).map_err(err)?)?;
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
    let path = destinations[&bundle.root_id].to_string_lossy().to_string();
    let registration = register(home, &id, &path, &cwd, &bundle.title);
    let (registered,message) = match registration {
        Ok(()) => (true,"导入完成，已通过 Codex app-server 注册。可在 Codex 会话列表中继续；列表未刷新时请重新打开 Codex。".into()),
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
fn register(home: &Path, id: &str, path: &str, cwd: &str, title: &str) -> Result<(), String> {
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
            json!({"clientInfo":{"name":"codexui","version":"0.1.1"},"capabilities":{"experimentalApi":true}}),
        )?;
        let response = request(
            2,
            "thread/resume",
            json!({"threadId":id,"path":path,"cwd":cwd,"excludeTurns":true}),
        )?;
        if response["thread"]["id"] != id {
            return Err("Codex 返回了不同的会话 ID".into());
        }
        if !title.is_empty() {
            request(3, "thread/name/set", json!({"threadId":id,"name":title}))?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
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
