import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const script = readFileSync(new URL('../src-tauri/src/session_menu.js', import.meta.url), 'utf8');
const id = '8f2171f2-7a76-4c57-8b23-6a1c0b95cf01';
function setup(body, loading = false) {
  const dom = new JSDOM(`<!doctype html><html><head></head><body>${body}</body></html>`, { runScripts: 'outside-only', url: 'https://codexui.test' });
  const w = dom.window;
  Object.defineProperty(w.document, 'readyState', { configurable: true, value: loading ? 'loading' : 'complete' });
  const calls = []; const timers = [];
  w.__codexuiSessionAction = value => calls.push(JSON.parse(value));
  w.setTimeout = callback => { timers.push(callback); return timers.length; };
  w.eval(script);
  return { w, calls, flush() { while (timers.length) timers.shift()(); }, close() { w.close(); } };
}
function rightClick(env, selector) {
  env.w.document.querySelector(selector).dispatchEvent(new env.w.MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 100, clientY: 100 }));
  env.flush();
}
test('right click targets the clicked session and sends its UUID', () => {
  const env = setup(`<aside><a href="/threads/${id}"><span>迁移完整上下文</span></a></aside>`);
  rightClick(env, 'span');
  const menu = env.w.document.querySelector('[role="menu"]');
  assert.equal(menu.querySelectorAll('[role="menuitem"]').length, 2);
  menu.querySelector('[data-codexui-session-action="sync"]').click();
  assert.deepEqual(env.calls, [{ action: 'sync', id, title: '迁移完整上下文' }]);
  assert.match(env.w.document.querySelector('[role="status"]').textContent, /正在选择同步目录/);
  env.w.__codexuiSessionMenu.result('同步完成');
  assert.match(env.w.document.querySelector('[role="status"]').textContent, /同步完成/);
  env.close();
});
test('existing host menu items remain intact', () => {
  const env = setup(`<aside><a href="/local/${id}">Conversation</a></aside><div role="menu"><button role="menuitem" id="original">Archive</button></div>`);
  const menu = env.w.document.querySelector('[role="menu"]');
  menu.getBoundingClientRect = () => ({ width: 200 });
  rightClick(env, 'a');
  assert.equal(env.w.document.getElementById('original').textContent, 'Archive');
  assert.equal(menu.querySelectorAll('[data-codexui-session-action]').length, 2);
  env.close();
});
test('sidebar empty space offers import and unrelated content is untouched', () => {
  const env = setup('<aside id="sidebar"></aside><main>Message content</main>');
  rightClick(env, 'main'); assert.equal(env.w.document.querySelector('[role="menu"]'), null);
  rightClick(env, 'aside');
  assert.equal(env.w.document.querySelectorAll('[data-codexui-session-action]').length, 1);
  env.w.document.querySelector('[data-codexui-session-action="import"]').click();
  assert.deepEqual(env.calls, [{ action: 'import' }]);
  env.close();
});
test('keyboard navigation, escape and disposal restore the host document', () => {
  const env = setup(`<aside><a href="/threads/${id}">Chat</a></aside>`);
  env.w.document.querySelector('a').focus(); rightClick(env, 'a');
  const first = env.w.document.activeElement;
  first.dispatchEvent(new env.w.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  assert.equal(env.w.document.activeElement.dataset.codexuiSessionAction, 'import');
  env.w.document.dispatchEvent(new env.w.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(env.w.document.querySelector('[role="menu"]'), null);
  assert.equal(env.w.document.activeElement.tagName, 'A');
  env.w.__codexuiSessionMenu.dispose(); rightClick(env, 'a');
  assert.equal(env.w.document.querySelector('[role="menu"]'), null);
  assert.equal(env.w.document.getElementById('codexui-session-menu-style'), null);
  env.close();
});
test('document-start injection waits for DOM readiness', () => {
  const env = setup('<aside></aside>', true);
  assert.equal(env.w.__codexuiSessionMenu, undefined);
  Object.defineProperty(env.w.document, 'readyState', { value: 'interactive' });
  env.w.document.dispatchEvent(new env.w.Event('DOMContentLoaded'));
  assert.ok(env.w.__codexuiSessionMenu);
  env.close();
});
test('disconnected bridge gives persistent feedback without sending', () => {
  const env = setup(`<aside><a href="/threads/${id}">Chat</a></aside>`);
  delete env.w.__codexuiSessionAction; rightClick(env, 'a');
  env.w.document.querySelector('[data-codexui-session-action="sync"]').click();
  assert.match(env.w.document.querySelector('[role="status"]').textContent, /连接已断开/);
  assert.equal(env.calls.length, 0);
  env.close();
});
