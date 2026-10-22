import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { releaseMetadata } from '../scripts/prepare-release.mjs';

function fixture(t, { cargo = '0.2.0', notes = '## [0.2.0] - 2026-10-23\n\n完整上下文迁移\n\n## [0.1.1]\n旧版本内容' } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'codexui-release-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'src-tauri'));
  writeFileSync(join(root, 'package.json'), JSON.stringify({ version: '0.2.0' }));
  writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '0.2.0' }));
  writeFileSync(join(root, 'src-tauri/Cargo.toml'), `[package]\nname = "codex-ui"\nversion = "${cargo}"\n`);
  writeFileSync(join(root, 'src-tauri/Cargo.lock'), 'name = "codex-ui"\nversion = "0.2.0"\n');
  writeFileSync(join(root, 'CHANGELOG.md'), notes);
  return root;
}
test('release notes contain only the matching version section', t => {
  assert.deepEqual(releaseMetadata(fixture(t)), { version: '0.2.0', tag: 'v0.2.0', body: '完整上下文迁移' });
});
test('release preparation refuses mismatched package versions', t => {
  assert.throws(() => releaseMetadata(fixture(t, { cargo: '0.1.1' })), /versions disagree/);
});
test('release preparation refuses missing changelog entries', t => {
  assert.throws(() => releaseMetadata(fixture(t, { notes: '## [0.1.1]\nold notes' })), /Missing release notes/);
});
