import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderSync, FolderInput, MousePointer2, RefreshCw, X, Search } from "lucide-react";

type Session = { id: string; title: string; cwd: string; timestamp: string };
type Binding = { id: string; directory: string; lastError: string | null };
type Preview = { title: string; sourceCwd: string; files: number; threads: number; agents: number; bytes: number; snapshot: string };
type ImportResult = { id: string; path: string; registered: boolean; message: string };
const COPY = {
  "zh-CN": {
    threads: "会话", agents: "子 Agent",
    title: "会话同步与迁移", hint: "电脑 A 同步目录 → 同步软件 → 电脑 B 导入，继续完整上下文。",
    menu: "启用 Codex 右键菜单", import: "导入会话", refresh: "刷新", search: "搜索会话、项目路径或 UUID",
    sync: "同步到目录", stop: "停止同步", loading: "正在处理…", empty: "没有匹配的本机会话", select: "选择会话迁移包目录（codex-session-UUID）",
    target: "选择同步目录", project: "选择这台电脑上的项目目录", preview: "导入预览", source: "来源项目", destination: "本机项目", chooseProject: "选择项目目录",
    context: "历史文件", confirm: "导入并创建独立会话", cancel: "取消", connected: "正在连接右键菜单…", active: "自动同步中", status: "会话操作结果",
    note: "CodexUI 运行期间每 15 秒单向更新同步目录。电脑 B 导入为独立副本，可反复导入新快照；项目源码和外部附件需另行同步。",
    privacy: "迁移包含完整对话、工具输出与历史指令，请选择可信目录。不会复制账户凭据或修改已有会话。",
    ready: "已同步，CodexUI 运行期间将持续更新：", stopped: "已停止自动同步，目录内的快照已保留。", importHint: "保留历史指令与消息，重新分配 UUID，并把项目元数据映射到所选目录。",
  },
  en: {
    threads: "Threads", agents: "Sub-agents",
    title: "Session sync & transfer", hint: "Sync on computer A → your sync software → import on computer B and continue with context.",
    menu: "Enable Codex context menu", import: "Import session", refresh: "Refresh", search: "Search sessions, project paths or UUIDs",
    sync: "Sync to folder", stop: "Stop sync", loading: "Working…", empty: "No matching local sessions", select: "Choose a session package folder (codex-session-UUID)",
    target: "Choose a sync folder", project: "Choose the project folder on this computer", preview: "Review import", source: "Source project", destination: "Local project", chooseProject: "Choose project folder",
    context: "History files", confirm: "Import as an independent session", cancel: "Cancel", connected: "Connecting context menu…", active: "Auto sync enabled", status: "Session transfer result",
    note: "While CodexUI runs, snapshots update every 15 seconds in one direction. Imports are independent copies; newer snapshots can be imported again. Sync project code and external attachments separately.",
    privacy: "Packages contain conversations, tool output and historical instructions. Choose a trusted folder. Account credentials are excluded and existing chats are preserved.",
    ready: "Synced. CodexUI will keep this folder updated while running: ", stopped: "Automatic sync stopped. Existing snapshots were kept.", importHint: "Retains historical instructions and messages, assigns new UUIDs and maps project metadata to your chosen folder.",
  },
};

export default function SessionTransfer({ language, connected }: { language: "en" | "zh-CN"; connected: boolean }) {
  const t = COPY[language];
  const [sessions, setSessions] = useState<Session[]>([]);
  const [bindings, setBindings] = useState<Binding[]>([]);
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [menuStatus, setMenuStatus] = useState("");
  const [pending, setPending] = useState<{ directory: string; preview: Preview } | null>(null);
  const [project, setProject] = useState("");
  const refresh = async () => {
    const [s, b] = await Promise.all([invoke<Session[]>("list_sessions"), invoke<Binding[]>("list_syncs")]);
    setSessions(s); setBindings(b);
  };
  useEffect(() => {
    refresh().catch(e => setError(String(e)));
    const subscriptions = [
      listen<string>("session-menu-status", e => setMenuStatus(e.payload)),
      listen<string>("session-transfer-result", e => { setMessage(e.payload); refresh().catch(e => setError(String(e))); }),
      listen<Binding[]>("session-sync-updated", e => setBindings(e.payload)),
    ];
    return () => { subscriptions.forEach(p => p.then(unlisten => unlisten()).catch(() => {})); };
  }, []);
  useEffect(() => {
    if (connected && localStorage.getItem("codexui-session-menu") === "enabled") {
      invoke("enable_session_menu").catch(e => setError(String(e)));
    }
  }, [connected]);
  const action = async (work: () => Promise<void>) => {
    setBusy(true); setError(""); setMessage("");
    try { await work(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const pick = async (title: string) => {
    const selected = await open({ directory: true, multiple: false, title });
    return typeof selected === "string" ? selected : null;
  };
  const sync = (session: Session) => action(async () => {
    const directory = await pick(t.target); if (!directory) return;
    const path = await invoke<string>("sync_session", { id: session.id, directory, title: session.title });
    setMessage(t.ready + path); await refresh();
  });
  const beginImport = () => action(async () => {
    const directory = await pick(t.select); if (!directory) return;
    const preview = await invoke<Preview>("preview_import", { directory });
    setProject(""); setPending({ directory, preview });
  });
  const doImport = () => action(async () => {
    if (!pending || !project) return;
    const result = await invoke<ImportResult>("import_session", { directory: pending.directory, projectDirectory: project, snapshot: pending.preview.snapshot });
    setMessage(`${result.message}\nUUID: ${result.id}\n${result.path}`); setPending(null); await refresh();
  });
  const visible = sessions.filter(s => `${s.title} ${s.cwd} ${s.id}`.toLowerCase().includes(query.toLowerCase()));
  return <section className="session-panel" aria-labelledby="session-title">
    <div className="section-header"><div><div className="eyebrow">CONTEXT · LOCAL FIRST</div><h2 id="session-title">{t.title}</h2><p>{t.hint}</p></div>
      <button className="button button-secondary" disabled={busy} onClick={beginImport}><FolderInput size={16} />{t.import}</button></div>
    <div className="session-toolbar">
      <button className="button button-quiet" disabled={busy || !connected} onClick={() => action(async () => { setMenuStatus(t.connected); await invoke("enable_session_menu"); localStorage.setItem("codexui-session-menu", "enabled"); })}><MousePointer2 size={16} />{t.menu}</button>
      <button className="button button-quiet" disabled={busy} onClick={() => action(refresh)}><RefreshCw size={16} />{t.refresh}</button>
      {menuStatus && <span className="session-menu-status" role="status">{menuStatus}</span>}
    </div>
    <label className="session-search"><Search size={16} /><input type="search" value={query} onChange={e => setQuery(e.target.value)} placeholder={t.search} aria-label={t.search} /></label>
    <div className="session-list" aria-busy={busy}>
      {visible.map(s => { const binding = bindings.find(b => b.id === s.id); return <article className="session-row" key={s.id}>
        <div className="session-row-copy"><strong title={s.title}>{s.title}</strong><span title={s.cwd}>{s.cwd}</span><small>{s.timestamp.slice(0, 10)} · {s.id}</small>
          {binding && <div className={binding.lastError ? "session-warning" : "session-sync-state"}>{binding.lastError || t.active}<br /><span>{binding.directory}</span></div>}</div>
        <div className="actions"><button className="button button-quiet" disabled={busy} onClick={() => sync(s)}><FolderSync size={16} />{t.sync}</button>
          {binding && <button className="icon-button" aria-label={t.stop} title={t.stop} disabled={busy} onClick={() => action(async () => { await invoke("remove_sync", { id: s.id }); setMessage(t.stopped); await refresh(); })}><X size={16} /></button>}</div>
      </article>; })}
      {!visible.length && <p className="session-empty">{t.empty}</p>}
    </div>
    {busy && <p className="session-notice" role="status">{t.loading}</p>}
    {error && <p className="error" role="alert">{error}</p>}
    {message && <div className="session-notice" role="status" aria-label={t.status}>{message}</div>}
    <p className="session-help">{t.note}</p><p className="session-help">{t.privacy}</p>
    {pending && <dialog className="session-import-dialog" aria-labelledby="import-title" onCancel={e => { e.preventDefault(); if (!busy) setPending(null); }} ref={element => { if (element && !element.open) element.showModal(); }}>
      <h2 id="import-title">{t.preview}</h2><h3>{pending.preview.title}</h3>
      <dl><dt>{t.source}</dt><dd>{pending.preview.sourceCwd}</dd><dt>{t.threads}</dt><dd>{pending.preview.threads} · {pending.preview.agents} {t.agents}</dd><dt>{t.context}</dt><dd>{pending.preview.files} · {(pending.preview.bytes / 1024 / 1024).toFixed(2)} MiB</dd><dt>{t.destination}</dt><dd>{project || "—"}</dd></dl>
      <p>{t.importHint}</p><button className="button button-secondary" disabled={busy} onClick={() => action(async () => { const p = await pick(t.project); if (p) setProject(p); })}><FolderInput size={16} />{t.chooseProject}</button>
      <div className="actions"><button className="button button-quiet" disabled={busy} onClick={() => setPending(null)}>{t.cancel}</button><button className="button button-primary" disabled={busy || !project} onClick={doImport}>{busy ? t.loading : t.confirm}</button></div>
      {error && <p role="alert" className="session-warning">{error}</p>}
    </dialog>}
  </section>;
}
