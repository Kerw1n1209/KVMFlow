const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { JSDOM } = require('jsdom');
const { config, initializeI18n } = require('./i18n-helper');

async function locale(t, languages, preference = 'auto') {
  const dom = new JSDOM('<html><body><h1 data-i18n="settings"></h1></body></html>', {
    url: 'http://tauri.localhost', runScripts: 'outside-only',
  });
  t.after(() => dom.window.close());
  Object.defineProperty(dom.window.navigator, 'languages', { value: languages, configurable: true });
  await initializeI18n(dom.window, preference);
  return dom.window;
}

test('catalogs contain all referenced keys and matching interpolation parameters', () => {
  const zh = config.messages['zh-CN'], en = config.messages.en;
  assert.deepEqual(Object.keys(zh).sort(), Object.keys(en).sort());
  assert.ok(Object.keys(en).length > 250);
  const parameters = value => [...value.matchAll(/\{([a-zA-Z0-9_]+)\}/g)].map(match => match[1]).sort();
  for (const key of Object.keys(zh)) {
    assert.ok(zh[key].trim(), key);
    assert.ok(en[key].trim(), key);
    assert.deepEqual(parameters(zh[key]), parameters(en[key]), key);
    assert.doesNotMatch(en[key], /[\u3400-\u9fff]/, key);
  }
  const renderer = path.resolve(__dirname, '../src/renderer');
  for (const id of Object.values(config.legacyMessages || {})) assert.ok(Object.hasOwn(en, id));
  for (const name of ['shell.js', 'app.js', 'updates.js']) {
    const source = fs.readFileSync(path.join(renderer, name), 'utf8');
    for (const match of source.matchAll(/\b(?:t|message)\(["']([^"']+)["']/g)) {
      assert.ok(Object.hasOwn(en, match[1]), `${name}: ${match[1]}`);
    }
  }
  const document = new JSDOM(fs.readFileSync(path.join(renderer, 'index.html'), 'utf8')).window.document;
  for (const element of document.querySelectorAll('*')) {
    for (const attribute of element.attributes) {
      if (attribute.name.startsWith('data-i18n')) assert.ok(Object.hasOwn(en, attribute.value), attribute.value);
    }
  }
  for (const option of document.querySelectorAll('select option')) {
    assert.ok(option.dataset.i18n || option.lang || /^\d+$/.test(option.textContent), option.textContent);
  }
});

test('system languages choose Chinese or English and unsupported languages fall back to English', async t => {
  const win = await locale(t, ['zh-Hant-TW', 'en-US']);
  assert.equal(win.KVMFlowI18n.locale, 'zh-CN');
  assert.equal(win.document.querySelector('h1').textContent, '设置');
  Object.defineProperty(win.navigator, 'languages', { value: ['fr-FR'] });
  win.dispatchEvent(new win.Event('languagechange'));
  assert.equal(win.document.documentElement.lang, 'en');
  assert.equal(win.document.querySelector('h1').textContent, 'Settings');
  win.KVMFlowI18n.setPreference('zh-CN');
  win.dispatchEvent(new win.Event('languagechange'));
  assert.equal(win.KVMFlowI18n.locale, 'zh-CN');
});

test('explicit preference overrides the system and is restored from browser storage', async t => {
  const win = await locale(t, ['zh-CN'], 'en');
  win.KVMFlowI18n.setPreference('en');
  assert.equal(win.localStorage.getItem('kvmflow-language-v1'), 'en');
  win.eval(fs.readFileSync(path.resolve(__dirname, '../src/renderer/i18n.js'), 'utf8'));
  await win.KVMFlowI18n.ready;
  assert.equal(win.KVMFlowI18n.preference, 'en');
  assert.equal(win.KVMFlowI18n.locale, 'en');
});

test('configuration controls the initial default and interpolation preserves literal user values', async t => {
  const win = await locale(t, ['en-US']);
  win.KVMFLOW_I18N_CONFIG.defaultLanguage = 'zh-CN';
  win.eval(fs.readFileSync(path.resolve(__dirname, '../src/renderer/i18n.js'), 'utf8'));
  await win.KVMFlowI18n.ready;
  assert.equal(win.KVMFlowI18n.preference, 'zh-CN');
  win.KVMFlowI18n.setPreference('en');
  const name = '工作电脑 $& <script>alert(1)</script>';
  assert.equal(win.KVMFlowI18n.t('native.delete.body', { name }), `Delete “${name}”? This removes its port and input values.`);
  assert.equal(win.KVMFlowI18n.t('does.not.exist'), 'does.not.exist');
});
