const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.join(__dirname, '..');
const renderer = path.join(root, 'src/renderer');
const fonts = path.join(renderer, 'assets/fonts');
const messages = JSON.parse(
  fs.readFileSync(path.join(renderer, 'i18n/messages.json'), 'utf8'),
);
const manifest = JSON.parse(
  fs.readFileSync(path.join(root, 'scripts/font-subset.json'), 'utf8'),
);
const covered = new Set(manifest.characters);

function strings(value, out = []) {
  if (typeof value === 'string') out.push(value);
  else if (Array.isArray(value)) value.forEach((item) => strings(item, out));
  else if (value && typeof value === 'object') {
    for (const [key, item] of Object.entries(value)) {
      strings(key, out);
      strings(item, out);
    }
  }
  return out;
}

test('font subset covers every character in messages.json', () => {
  const missing = new Set();
  for (const text of strings(messages)) {
    for (const char of text) {
      if (!covered.has(char)) missing.add(char);
    }
  }
  assert.deepEqual([...missing], [], `missing glyphs: ${[...missing].join('')}`);
});

test('font subset covers ASCII, common punctuation, and GB2312 level 1', () => {
  for (let code = 0x20; code <= 0x7e; code += 1) {
    assert.ok(covered.has(String.fromCharCode(code)), `missing ASCII ${code}`);
  }
  for (const char of '·—–…“”‘’、。，！？；：（）【】《》「」') {
    assert.ok(covered.has(char), `missing punctuation ${char}`);
  }

  const decoder = new TextDecoder('gbk');
  let count = 0;
  for (let high = 0xb0; high <= 0xd7; high += 1) {
    const last = high === 0xd7 ? 0xf9 : 0xfe;
    for (let low = 0xa1; low <= last; low += 1) {
      let char;
      try {
        char = decoder.decode(Uint8Array.of(high, low));
      } catch {
        continue;
      }
      if (char === '\ufffd' || char.length !== 1) continue;
      count += 1;
      assert.ok(covered.has(char), `missing GB2312 level-1 ${char}`);
    }
  }
  assert.equal(count, 3755);
});

test('renderer bundles the fonts with swap, a packaged-first stack, and local CSP', () => {
  const css = fs.readFileSync(path.join(renderer, 'app.css'), 'utf8');
  for (const [family, file] of [
    ['KVMFlow Serif', 'KVMFlowSerif-Variable.woff2'],
    ['Noto Serif SC', 'NotoSerifSC-Variable.woff2'],
  ]) {
    const block = css.match(
      new RegExp(`@font-face\\s*\\{[^}]*font-family:\\s*"${family}"[^}]*\\}`),
    );
    assert.ok(block, `missing @font-face for ${family}`);
    assert.match(block[0], /font-display:\s*swap/);
    assert.ok(block[0].includes(`assets/fonts/${file}`));
    assert.ok(fs.statSync(path.join(fonts, file)).size > 0);
  }

  const stack = css.match(/--font-family:\s*([^;]+);/)[1];
  assert.ok(
    stack.indexOf('"KVMFlow Serif"') >= 0
      && stack.indexOf('"KVMFlow Serif"') < stack.indexOf('Charter'),
    'bundled Latin font must precede Charter',
  );
  assert.ok(
    stack.indexOf('"Noto Serif SC"') >= 0
      && stack.indexOf('"Noto Serif SC"') < stack.indexOf('Charter'),
    'bundled CJK font must precede Charter',
  );

  const shell = fs.readFileSync(path.join(renderer, 'shell.css'), 'utf8');
  assert.doesNotMatch(shell, /font-family/, 'shell.css must not override the app font stack');

  const navState = css.match(/\.nav \.nav-state \{([^}]*)\}/)[1];
  assert.match(navState, /white-space:\s*nowrap/);
  assert.match(navState, /text-overflow:\s*ellipsis/);
  assert.doesNotMatch(navState, /white-space:\s*normal/);

  const tauri = JSON.parse(
    fs.readFileSync(path.join(root, 'src-tauri/tauri.conf.json'), 'utf8'),
  );
  assert.match(tauri.app.security.csp, /font-src 'self'/);
  assert.equal(tauri.build.frontendDist, '../src/renderer');
});

test('bundled font files match the subset manifest', () => {
  let total = 0;
  for (const font of manifest.fonts) {
    const file = path.join(fonts, font.file);
    const bytes = fs.readFileSync(file);
    total += bytes.length;
    assert.equal(bytes.length, font.bytes);
    assert.equal(
      crypto.createHash('sha256').update(bytes).digest('hex'),
      font.sha256,
      `${font.file} does not match the manifest; regenerate the subsets`,
    );
  }
  assert.ok(total < 3 * 1024 * 1024, `fonts total ${total} bytes`);
});

test('the woff2 cmap actually contains the subset characters', (t) => {
  const probe = spawnSync('python3', ['-c', 'import fontTools'], { encoding: 'utf8' });
  if (probe.error || probe.status !== 0) {
    t.skip('python3 with fontTools is unavailable');
    return;
  }
  const script = [
    'import json, sys',
    'from fontTools.ttLib import TTFont',
    "manifest = json.load(open('scripts/font-subset.json', encoding='utf-8'))",
    "font = TTFont('src/renderer/assets/fonts/NotoSerifSC-Variable.woff2')",
    'cmap = set()',
    "for table in font['cmap'].tables: cmap.update(table.cmap.keys())",
    "missing = [c for c in manifest['characters'] if ord(c) not in cmap]",
    "print('missing: ' + ''.join(missing)) if missing else None",
    'sys.exit(1 if missing else 0)',
  ].join('\n');
  const result = spawnSync('python3', ['-c', script], {
    cwd: root,
    encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});
