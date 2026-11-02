const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const { JSDOM } = require('jsdom');

const source = fs.readFileSync(path.resolve(__dirname, '../src/renderer/select.js'), 'utf8');

function page(t, html = `<label for="pick">Timing</label>
  <select id="pick"><option value="a">One</option><option value="b" selected>Two</option><option value="c" disabled>Off</option><option value="d">Four</option></select>
  <button id="other">Other</button>`) {
  const dom = new JSDOM(`<!doctype html><body>${html}</body>`, { runScripts: 'outside-only' });
  t.after(() => dom.window.close());
  dom.window.eval(source);
  const select = dom.window.document.getElementById('pick');
  const wrap = select.parentElement;
  return { dom, window: dom.window, document: dom.window.document, select, wrap,
    trigger: wrap.querySelector('.neo-select-trigger'), list: wrap.querySelector('.neo-select-list') };
}
const settle = window => new Promise(resolve => window.setTimeout(resolve));
const key = (window, name) => new window.KeyboardEvent('keydown', { key: name, bubbles: true, cancelable: true });
const rows = list => [...list.querySelectorAll('[role="option"]')];

test('the native select stays in place and the trigger shows its value', t => {
  const { select, wrap, trigger, list } = page(t);
  assert.equal(wrap.className, 'neo-select');
  assert.equal(select.parentElement, wrap);
  assert.equal(select.tabIndex, -1);
  assert.equal(trigger.getAttribute('role'), 'combobox');
  assert.equal(trigger.getAttribute('aria-label'), 'Timing');
  assert.equal(trigger.textContent, 'Two');
  assert.equal(list.hidden, true);
});

test('opening lists enabled options and marks the current one', t => {
  const { trigger, list } = page(t);
  trigger.click();
  assert.equal(list.hidden, false);
  assert.equal(trigger.getAttribute('aria-expanded'), 'true');
  assert.deepEqual(rows(list).map(row => row.textContent), ['One', 'Two', 'Four']);
  assert.deepEqual(rows(list).map(row => row.getAttribute('aria-selected')), ['false', 'true', 'false']);
  assert.equal(rows(list)[1].querySelectorAll('svg').length, 1);
  assert.ok(rows(list)[1].hasAttribute('data-highlighted'));
  trigger.click();
  assert.equal(list.hidden, true);
});

test('choosing fires the input and change events existing code listens for', t => {
  const { select, trigger, list } = page(t);
  const events = [];
  select.addEventListener('input', () => events.push(`input:${select.value}`));
  select.addEventListener('change', () => events.push(`change:${select.value}`));
  trigger.click();
  rows(list)[2].click();
  assert.deepEqual(events, ['input:d', 'change:d']);
  assert.equal(trigger.textContent, 'Four');
  assert.equal(list.hidden, true);
});

test('choosing the current option fires nothing', t => {
  const { select, trigger, list } = page(t);
  let fired = 0;
  select.addEventListener('change', () => { fired += 1; });
  trigger.click();
  rows(list)[1].click();
  assert.equal(fired, 0);
});

test('the keyboard opens, moves, chooses and escapes', t => {
  const { window, select, trigger, list } = page(t);
  trigger.dispatchEvent(key(window, 'ArrowDown'));
  assert.equal(list.hidden, false);
  trigger.dispatchEvent(key(window, 'ArrowDown'));
  assert.ok(rows(list)[2].hasAttribute('data-highlighted'));
  trigger.dispatchEvent(key(window, 'ArrowDown'));
  assert.ok(rows(list)[2].hasAttribute('data-highlighted'), 'stops at the last row');
  trigger.dispatchEvent(key(window, 'Home'));
  assert.ok(rows(list)[0].hasAttribute('data-highlighted'));
  trigger.dispatchEvent(key(window, 'Enter'));
  assert.equal(select.value, 'a');
  assert.equal(list.hidden, true);
  trigger.dispatchEvent(key(window, ' '));
  assert.equal(list.hidden, false);
  trigger.dispatchEvent(key(window, 'Escape'));
  assert.equal(list.hidden, true);
  assert.equal(select.value, 'a');
});

test('a value set from code reaches the trigger', t => {
  const { select, trigger } = page(t);
  select.value = 'd';
  assert.equal(trigger.textContent, 'Four');
  select.selectedIndex = 0;
  assert.equal(trigger.textContent, 'One');
});

test('rebuilt options, relabelled text and disabling reach the trigger', async t => {
  const { window, select, trigger, list } = page(t);
  select.innerHTML = '<option value="x">Fresh</option>';
  await settle(window);
  assert.equal(trigger.textContent, 'Fresh');
  select.options[0].textContent = 'Translated';
  await settle(window);
  assert.equal(trigger.textContent, 'Translated');
  select.disabled = true;
  await settle(window);
  assert.equal(trigger.disabled, true);
  trigger.click();
  assert.equal(list.hidden, true);
});

test('selects added later are enhanced once', async t => {
  const { window, document } = page(t);
  const box = document.createElement('div');
  box.innerHTML = '<select id="late"><option>Late</option></select>';
  document.body.append(box);
  await settle(window);
  const late = document.getElementById('late');
  assert.equal(late.parentElement.className, 'neo-select');
  assert.equal(late.parentElement.querySelector('.neo-select-trigger').textContent, 'Late');
  const wrap = late.parentElement;
  wrap.remove();
  document.body.append(wrap);
  await settle(window);
  assert.equal(document.querySelectorAll('.neo-select-trigger').length, 2);
});

test('clicking elsewhere, focusing a label and removal behave', async t => {
  const { window, document, select, wrap, trigger, list } = page(t);
  trigger.click();
  document.getElementById('other').dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  assert.equal(list.hidden, true);
  select.focus();
  assert.equal(document.activeElement, trigger);
  trigger.click();
  wrap.remove();
  await settle(window);
  assert.equal(list.hidden, true);
});

test('only one list is open at a time', t => {
  const { document, trigger, list } = page(t, `<select id="pick"><option>A</option></select><select id="two"><option>B</option></select>`);
  const second = document.getElementById('two').parentElement;
  trigger.click();
  second.querySelector('.neo-select-trigger').click();
  assert.equal(list.hidden, true);
  assert.equal(second.querySelector('.neo-select-list').hidden, false);
});
