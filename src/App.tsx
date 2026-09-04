import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { Check, ChevronDown, Globe2, ImageOff, ImagePlus, MonitorUp, Play, RefreshCw, SlidersHorizontal, Sparkles, Trash2, Wifi, WifiOff } from "lucide-react";
import type { CdpStatus, Wallpaper } from "./types";

const EMPTY: CdpStatus = { running: false, connected: false, port: 9335, message: "正在检测运行环境…" };

type Language = "en" | "zh-CN";
const COPY = {
  en: {
    language: "Language", settings: "Appearance settings", connection: "CONNECTION",
    connected: "Connected — ready for wallpapers", reconnect: "Reconnect required", waiting: "Connect to ChatGPT",
    connectedDetail: "ChatGPT Desktop is ready to receive appearance changes.", runningDetail: "ChatGPT is running without an active CDP connection.", waitingDetail: "Launch ChatGPT with the secure local connection enabled.", restartingDetail: "Closing the current instance and reconnecting…",
    check: "Check status", complete: "Connected", connecting: "Connecting", restart: "Restart & connect", launch: "Launch & connect",
    failed: "Action failed", appearance: "Appearance", appearanceHint: "Changes apply the next time you select a wallpaper",
    opacity: "Wallpaper opacity", blur: "Background blur", dim: "Dark overlay", library: "WALLPAPER LIBRARY", wallpapers: "My wallpapers",
    clear: "Remove wallpaper", add: "Add images", inUse: "In use", clickApply: "Click to apply", apply: "Apply wallpaper", remove: "Delete",
    addMore: "Add more wallpapers", addFirst: "Add your first wallpaper", localOnly: "Only affects ChatGPT Desktop windows", images: "Images",
    confirm: "ChatGPT must fully restart to enable CDP.\n\nThis will close every ChatGPT / Codex window. Save any work in progress before continuing. Restart now?"
  },
  "zh-CN": {
    language: "语言", settings: "显示外观设置", connection: "连接状态",
    connected: "已连接，可以切换壁纸", reconnect: "需要重新连接", waiting: "等待连接 ChatGPT",
    connectedDetail: "ChatGPT 桌面端已准备好接收外观更改。", runningDetail: "ChatGPT 正在运行，但尚未建立 CDP 连接。", waitingDetail: "启动 ChatGPT 并启用安全的本地连接。", restartingDetail: "正在关闭当前实例并重新连接…",
    check: "检测状态", complete: "连接完成", connecting: "正在连接", restart: "重启并连接", launch: "启动并连接",
    failed: "操作未完成", appearance: "显示效果", appearanceHint: "参数会在下一次应用壁纸时生效",
    opacity: "壁纸透明度", blur: "背景模糊", dim: "暗色遮罩", library: "壁纸库", wallpapers: "我的壁纸",
    clear: "取消壁纸", add: "添加图片", inUse: "正在使用", clickApply: "点击应用", apply: "应用壁纸", remove: "删除",
    addMore: "添加更多壁纸", addFirst: "添加第一张壁纸", localOnly: "仅影响 ChatGPT 桌面窗口", images: "图片",
    confirm: "需要完全重启 ChatGPT 才能启用 CDP。\n\n这会关闭所有 ChatGPT / Codex 窗口，请先保存正在进行的工作。是否继续？"
  }
} as const;

type ControlProps = { label: string; value: number; suffix: string; min: number; max: number; onChange: (value: number) => void };

function Control({ label, value, suffix, min, max, onChange }: ControlProps) {
  const progress = ((value - min) / (max - min)) * 100;
  return <label className="control">
    <span>{label}<strong>{value}{suffix}</strong></span>
    <input type="range" min={min} max={max} value={value}
      style={{ "--range-progress": `${progress}%` } as React.CSSProperties}
      onChange={(event) => onChange(Number(event.target.value))} />
  </label>;
}

export default function App() {
  const [language, setLanguage] = useState<Language>(() => (localStorage.getItem("codexui-language") as Language) || "en");
  const [items, setItems] = useState<Wallpaper[]>([]);
  const [active, setActive] = useState("");
  const [status, setStatus] = useState<CdpStatus>(EMPTY);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [opacity, setOpacity] = useState(72);
  const [blur, setBlur] = useState(0);
  const [dim, setDim] = useState(18);
  const controlsRef = useRef<HTMLElement>(null);
  const t = COPY[language];

  useEffect(() => {
    localStorage.setItem("codexui-language", language);
    document.documentElement.lang = language;
  }, [language]);

  const refresh = async () => {
    setError("");
    try { setStatus(await invoke("cdp_status")); } catch (caught) { setError(String(caught)); }
  };

  useEffect(() => {
    invoke<Wallpaper[]>("list_wallpapers").then(setItems).catch((caught) => setError(String(caught)));
    refresh();
  }, []);

  const launch = async () => {
    setBusy(true); setError("");
    try {
      if (status.running && !status.connected) {
        const confirmed = window.confirm(t.confirm);
        if (!confirmed) return;
        setStatus((current) => ({ ...current, message: t.restartingDetail }));
        setStatus(await invoke("restart_chatgpt_with_cdp"));
      } else setStatus(await invoke("launch_chatgpt"));
    } catch (caught) { setError(String(caught)); await refresh(); }
    finally { setBusy(false); }
  };

  const addPaths = async (paths: string[]) => {
    if (!paths.length) return;
    setBusy(true); setError("");
    try { setItems(await invoke<Wallpaper[]>("add_wallpapers", { paths })); }
    catch (caught) { setError(String(caught)); }
    finally { setBusy(false); }
  };

  const choose = async () => {
    const selected = await open({ multiple: true, filters: [{ name: t.images, extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp"] }] });
    if (selected) await addPaths(Array.isArray(selected) ? selected : [selected]);
  };

  const apply = async (id: string) => {
    setBusy(true); setError("");
    try { await invoke("apply_wallpaper", { id, opacity: opacity / 100, blur, dim: dim / 100 }); setActive(id); }
    catch (caught) { setError(String(caught)); }
    finally { setBusy(false); }
  };

  const remove = async (id: string) => {
    try { setItems(await invoke("remove_wallpaper", { id })); if (active === id) setActive(""); }
    catch (caught) { setError(String(caught)); }
  };

  const clear = async () => {
    setBusy(true); setError("");
    try { await invoke("clear_wallpaper"); setActive(""); }
    catch (caught) { setError(String(caught)); }
    finally { setBusy(false); }
  };

  const statusLabel = status.connected ? t.connected : status.running ? t.reconnect : t.waiting;
  const statusDetail = busy ? t.restartingDetail : status.connected ? t.connectedDetail : status.running ? t.runningDetail : t.waitingDetail;

  return <main className="app-shell">
    <header className="topbar">
      <div className="brand"><div className="brand-mark"><MonitorUp size={20} strokeWidth={1.8} /></div><div><h1>CodexUI</h1><p>Workspace appearance</p></div></div>
      <div className="header-actions">
        <label className="language-select" aria-label={t.language}><Globe2 size={15} /><select value={language} onChange={(event) => setLanguage(event.target.value as Language)}><option value="en">English</option><option value="zh-CN">简体中文</option></select><ChevronDown size={13} /></label>
        <button className="icon-button" type="button" aria-label={t.settings} onClick={() => controlsRef.current?.scrollIntoView({ behavior: "smooth" })}><SlidersHorizontal size={18} /></button>
      </div>
    </header>

    <section className={`connection-panel ${status.connected ? "is-connected" : ""}`}>
      <div className="connection-copy"><div className="eyebrow"><span className="status-dot" /> {t.connection}</div><h2>{statusLabel}</h2><p>{statusDetail}</p></div>
      <div className="actions">
        <button className="button button-quiet" type="button" onClick={refresh} disabled={busy}><RefreshCw size={16} className={busy ? "spin" : ""} />{t.check}</button>
        <button className="button button-primary" type="button" onClick={launch} disabled={busy || status.connected}><Play size={15} fill="currentColor" />{status.connected ? t.complete : busy ? t.connecting : status.running ? t.restart : t.launch}</button>
      </div>
    </section>

    {error && <div className="error" role="alert"><strong>{t.failed}</strong><span>{error}</span></div>}

    <section className="appearance-panel" ref={controlsRef}>
      <div className="section-intro"><span><Sparkles size={16} /></span><div><h3>{t.appearance}</h3><p>{t.appearanceHint}</p></div></div>
      <div className="controls">
        <Control label={t.opacity} value={opacity} suffix="%" min={20} max={100} onChange={setOpacity} />
        <Control label={t.blur} value={blur} suffix=" px" min={0} max={30} onChange={setBlur} />
        <Control label={t.dim} value={dim} suffix="%" min={0} max={70} onChange={setDim} />
      </div>
    </section>

    <section className="library">
      <div className="section-header">
        <div><div className="eyebrow">{t.library}</div><h2>{t.wallpapers} <span>{items.length}</span></h2></div>
        <div className="actions">
          <button className="button button-quiet" type="button" onClick={clear} disabled={busy || !active}><ImageOff size={16} />{t.clear}</button>
          <button className="button button-secondary" type="button" onClick={choose} disabled={busy}><ImagePlus size={16} />{t.add}</button>
        </div>
      </div>

      <div className="wallpaper-grid">
        {items.map((wallpaper) => <article key={wallpaper.id} className={`wallpaper-card ${active === wallpaper.id ? "is-active" : ""}`}>
          <button className="wallpaper-select" type="button" aria-label={`${t.apply} ${wallpaper.name}`} onClick={() => apply(wallpaper.id)} disabled={busy}><img src={wallpaper.dataUrl} alt="" /><span className="image-shade" /></button>
          <div className="card-meta"><span title={wallpaper.name}>{wallpaper.name}</span><small>{active === wallpaper.id ? t.inUse : t.clickApply}</small></div>
          <button className="delete-button" type="button" aria-label={`${t.remove} ${wallpaper.name}`} onClick={() => remove(wallpaper.id)}><Trash2 size={15} /></button>
          {active === wallpaper.id && <span className="active-mark"><Check size={14} strokeWidth={2.5} /></span>}
        </article>)}
        <button className="add-card" type="button" onClick={choose} disabled={busy}><span className="add-icon"><ImagePlus size={22} strokeWidth={1.7} /></span><strong>{items.length ? t.addMore : t.addFirst}</strong><small>PNG · JPG · WEBP · GIF</small></button>
      </div>
    </section>

    <footer className="footer"><span className={status.connected ? "connected" : ""}>{status.connected ? <Wifi size={14} /> : <WifiOff size={14} />}CDP · {status.port}</span><span>{t.localOnly}</span></footer>
  </main>;
}
