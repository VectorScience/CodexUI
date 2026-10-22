import { readFileSync, appendFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

export function releaseMetadata(root) {
  const read = name => readFileSync(resolve(root, name), 'utf8');
  const version = JSON.parse(read('package.json')).version;
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Release version must be a stable semantic version');
  const tauri = JSON.parse(read('src-tauri/tauri.conf.json')).version;
  const cargo = read('src-tauri/Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const lock = read('src-tauri/Cargo.lock').match(/name = "codex-ui"\r?\nversion = "([^"]+)"/)?.[1];
  if ([tauri, cargo, lock].some(value => value !== version)) throw new Error('Release versions disagree across package.json, Tauri, Cargo.toml and Cargo.lock');
  const section = read('CHANGELOG.md').split(/^## /m).find(value => value.startsWith(`[${version}]`));
  const body = section?.replace(/^[^\n]*\n/, '').trim();
  if (!body) throw new Error(`Missing release notes for ${version}`);
  return { version, tag: `v${version}`, body };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const { tag, body } = releaseMetadata(root);
  if (process.env.GITHUB_ACTIONS === 'true') {
    if (process.env.GITHUB_REF_TYPE === 'tag' && process.env.GITHUB_REF_NAME !== tag) throw new Error(`Expected tag ${tag}`);
    const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
    if (git(['rev-parse', `${tag}^{commit}`]) !== git(['rev-parse', 'HEAD'])) throw new Error(`${tag} must point to the checked-out release commit`);
  }
  if (process.env.GITHUB_OUTPUT) {
    const delimiter = 'CODEXUI_RELEASE_NOTES_END';
    if (body.split('\n').includes(delimiter)) throw new Error('Release notes contain output delimiter');
    appendFileSync(process.env.GITHUB_OUTPUT, `tag=${tag}\nbody<<${delimiter}\n${body}\n${delimiter}\n`);
  }
  process.stdout.write(`${tag}\n\n${body}\n`);
}
