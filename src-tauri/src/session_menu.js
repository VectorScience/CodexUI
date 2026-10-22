(function install() {
  if (document.readyState === 'loading') { document.addEventListener('DOMContentLoaded',install,{once:true}); return; }
  if (window.__codexuiSessionMenu) return;
  window.__codexuiSessionHeartbeat = Date.now();
  const idPattern = /(?:threads?|local|chats?)\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:[/?#]|$)/i;
  let selected = null;
  let pending = false;
  let fallback = null;
  let lastFocus = null;
  const style = document.createElement('style');
  style.id = 'codexui-session-menu-style';
  style.textContent = `
    .codexui-session-action{display:flex;align-items:center;gap:10px;width:100%;min-height:36px;padding:8px 12px;border:0;border-radius:5px;background:transparent;color:inherit;font:inherit;font-size:13px;text-align:left;cursor:pointer}
    .codexui-session-action:hover,.codexui-session-action:focus-visible{background:rgba(128,128,128,.18);outline:2px solid #75d0aa;outline-offset:-2px}
    .codexui-session-action:disabled{opacity:.5;cursor:wait}
    #codexui-session-fallback{position:fixed;z-index:2147483646;padding:5px;min-width:200px;border:1px solid #555;border-radius:9px;background:Canvas;color:CanvasText;box-shadow:0 8px 30px #0005;color-scheme:dark light}
    #codexui-session-feedback{position:fixed;bottom:24px;right:24px;max-width:min(540px,90vw);max-height:50vh;overflow:auto;z-index:2147483647;padding:16px 20px;border:1px solid #75d0aa;border-radius:12px;background:#161919;color:#f0f2f1;font:14px/1.6 system-ui;box-shadow:0 12px 40px #0007;white-space:pre-wrap}
    #codexui-session-feedback button{margin-left:16px;background:transparent;color:inherit;border:1px solid #777;border-radius:5px;cursor:pointer}
  `;
  document.head.appendChild(style);
  function feedback(message) {
    document.getElementById('codexui-session-feedback')?.remove();
    const box = document.createElement('div'); box.id = 'codexui-session-feedback'; box.setAttribute('role','status');
    const text = document.createElement('span'); text.textContent = message; box.appendChild(text);
    const close = document.createElement('button'); close.textContent = '关闭'; close.onclick = () => box.remove(); box.appendChild(close);
    document.body.appendChild(box);
  }
  function closeFallback() { fallback?.remove(); fallback = null; }
  function run(action, context) {
    if (pending) return;
    if (typeof window.__codexuiSessionAction !== 'function' || Date.now() - window.__codexuiSessionHeartbeat > 10000) { feedback('CodexUI 连接已断开，请在 CodexUI 中重新启用会话右键菜单。'); return; }
    pending = true; closeFallback();
    feedback(action === 'sync' ? '正在选择同步目录…' : '正在选择会话迁移包…');
    window.__codexuiSessionAction(JSON.stringify({action, id: context?.id, title: context?.title}));
  }
  function buttons(menu, context) {
    if (menu.querySelector('[data-codexui-session-action]')) return;
    for (const [action,label] of [['sync','同步到目录…'],['import','导入会话…']]) {
      if (action === 'sync' && !context) continue;
      const button = document.createElement('button'); button.type = 'button'; button.className = 'codexui-session-action';
      button.setAttribute('role','menuitem'); button.dataset.codexuiSessionAction = action; button.textContent = label;
      button.onclick = event => { event.preventDefault(); event.stopPropagation(); run(action,context); };
      button.onkeydown = event => {
        if (event.key === 'Escape') { closeFallback(); lastFocus?.focus(); }
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          const items = [...menu.querySelectorAll('[role="menuitem"]')].filter(e => !e.disabled);
          const i = items.indexOf(button); const next = (i + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length;
          items[next]?.focus(); event.preventDefault();
        }
      };
      menu.appendChild(button);
    }
  }
  function contextFor(target) {
    if (!(target instanceof Element)) return null;
    const row = target.closest('[data-thread-id],[data-conversation-id],a[href]');
    if (!row) return null;
    const raw = row.getAttribute('data-thread-id') || row.getAttribute('data-conversation-id');
    const id = raw && /^[0-9a-f-]{36}$/i.test(raw) ? raw : row.getAttribute('href')?.match(idPattern)?.[1];
    return id ? {id,title: (row.getAttribute('aria-label') || row.textContent || '').trim().slice(0,300)} : null;
  }
  const onContext = event => {
    closeFallback(); selected = contextFor(event.target); lastFocus = document.activeElement;
    if (!selected && !event.target.closest?.('aside,nav,[data-sidebar]')) return;
    event.preventDefault();
    const context = selected;
    // Give the host's normal context menu a chance to render before adding our entries.
    setTimeout(() => {
      const menu = [...document.querySelectorAll('[role="menu"]')].find(e => e.getBoundingClientRect().width > 0);
      if (menu) { buttons(menu,context); return; }
      fallback = document.createElement('div'); fallback.id = 'codexui-session-fallback'; fallback.setAttribute('role','menu');
      fallback.setAttribute('aria-label','CodexUI 会话迁移'); buttons(fallback,context); document.body.appendChild(fallback);
      fallback.style.left = Math.max(8,Math.min(event.clientX,innerWidth-fallback.offsetWidth-8))+'px';
      fallback.style.top = Math.max(8,Math.min(event.clientY,innerHeight-fallback.offsetHeight-8))+'px';
      fallback.querySelector('button')?.focus();
    },100);
  };
  const onPointer = event => { if (!fallback?.contains(event.target)) closeFallback(); };
  const onKey = event => { if (event.key === 'Escape') { closeFallback(); lastFocus?.focus(); } };
  document.addEventListener('contextmenu',onContext,true);
  document.addEventListener('pointerdown',onPointer,true);
  document.addEventListener('keydown',onKey,true);
  window.__codexuiSessionMenu = {
    result(message) { pending = false; feedback(message); },
    dispose() { document.removeEventListener('contextmenu',onContext,true); document.removeEventListener('pointerdown',onPointer,true); document.removeEventListener('keydown',onKey,true); closeFallback(); style.remove(); document.getElementById('codexui-session-feedback')?.remove(); delete window.__codexuiSessionMenu; }
  };
})();
