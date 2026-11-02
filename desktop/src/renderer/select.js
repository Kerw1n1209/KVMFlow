// neobrutalism.dev Select over every native <select>. The hidden native element
// stays the source of truth, so existing value reads and change listeners work.
(() => {
  const chevron = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg>';
  const check = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';
  const parts = new WeakMap();
  let current = null, nextId = 0;

  const enabledOptions = select => [...select.options].filter(option => !option.disabled);

  function sync(select) {
    const part = parts.get(select);
    if (!part) return;
    part.value.textContent = select.options[select.selectedIndex]?.textContent.trim() || '';
    part.trigger.disabled = select.disabled;
    const label = select.labels?.[0]?.textContent.trim() || select.getAttribute('aria-label');
    if (label) part.trigger.setAttribute('aria-label', label);
    else part.trigger.removeAttribute('aria-label');
  }

  // Scripts assign .value without firing change; keep the trigger in step.
  for (const name of ['value', 'selectedIndex']) {
    const native = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, name);
    Object.defineProperty(HTMLSelectElement.prototype, name, {
      ...native,
      set(value) { native.set.call(this, value); sync(this); },
    });
  }

  function highlight(index) {
    if (!current) return;
    current.index = index;
    current.part.list.querySelectorAll('[role="option"]').forEach((row, i) => {
      row.toggleAttribute('data-highlighted', i === index);
      if (i === index) {
        current.part.trigger.setAttribute('aria-activedescendant', row.id);
        row.scrollIntoView?.({ block: 'nearest' });
      }
    });
  }

  function place(part) {
    const box = part.trigger.getBoundingClientRect();
    const { list } = part;
    list.style.minWidth = `${box.width}px`;
    list.style.left = `${box.left}px`;
    const below = window.innerHeight - box.bottom - 12;
    const fitsBelow = list.offsetHeight <= below || below >= box.top - 12;
    list.style.maxHeight = `${Math.min(384, Math.max(fitsBelow ? below : box.top - 12, 80))}px`;
    list.style.top = fitsBelow ? `${box.bottom + 4}px` : `${box.top - list.offsetHeight - 4}px`;
  }

  function close({ focus = false } = {}) {
    if (!current) return;
    const { part } = current;
    current = null;
    part.list.hidden = true;
    part.list.innerHTML = '';
    part.trigger.setAttribute('aria-expanded', 'false');
    part.trigger.removeAttribute('aria-activedescendant');
    if (focus && part.trigger.isConnected) part.trigger.focus();
  }

  function open(select) {
    const part = parts.get(select);
    if (!part || select.disabled) return;
    close();
    const rows = enabledOptions(select);
    part.list.innerHTML = '';
    rows.forEach((option, i) => {
      const row = document.createElement('div');
      row.id = `${part.list.id}-${i}`;
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', String(option.selected));
      row.dataset.index = String(i);
      const text = document.createElement('span');
      text.textContent = option.textContent.trim();
      row.append(text);
      if (option.selected) row.insertAdjacentHTML('beforeend', check);
      part.list.append(row);
    });
    current = { select, part, rows, index: -1 };
    part.list.hidden = false;
    part.trigger.setAttribute('aria-expanded', 'true');
    place(part);
    highlight(Math.max(rows.findIndex(option => option.selected), 0));
  }

  function choose(index) {
    if (!current) return;
    const { select, rows } = current;
    const option = rows[index];
    close({ focus: true });
    if (!option || option.selected) return;
    select.value = option.value;
    select.dispatchEvent(new Event('input', { bubbles: true }));
    select.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function onKeydown(event, select) {
    const active = current?.select === select;
    if (!active) {
      if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(event.key)) { event.preventDefault(); open(select); }
      return;
    }
    const last = current.rows.length - 1;
    switch (event.key) {
      case 'ArrowDown': event.preventDefault(); highlight(Math.min(current.index + 1, last)); break;
      case 'ArrowUp': event.preventDefault(); highlight(Math.max(current.index - 1, 0)); break;
      case 'Home': event.preventDefault(); highlight(0); break;
      case 'End': event.preventDefault(); highlight(last); break;
      case 'Enter': case ' ': event.preventDefault(); choose(current.index); break;
      case 'Escape': event.preventDefault(); event.stopPropagation(); close({ focus: true }); break;
      case 'Tab': close(); break;
      default: break;
    }
  }

  function enhance(select) {
    if (parts.has(select)) return;
    const wrap = document.createElement('div');
    wrap.className = 'neo-select';
    const trigger = document.createElement('button');
    trigger.type = 'button';
    trigger.className = 'neo-select-trigger';
    trigger.setAttribute('role', 'combobox');
    trigger.setAttribute('aria-haspopup', 'listbox');
    trigger.setAttribute('aria-expanded', 'false');
    const value = document.createElement('span');
    value.className = 'neo-select-value';
    trigger.append(value);
    trigger.insertAdjacentHTML('beforeend', chevron);
    const list = document.createElement('div');
    list.className = 'neo-select-list';
    list.id = `neo-select-${++nextId}`;
    list.setAttribute('role', 'listbox');
    list.hidden = true;
    trigger.setAttribute('aria-controls', list.id);
    parts.set(select, { trigger, value, list });
    select.before(wrap);
    wrap.append(select, trigger, list);
    select.tabIndex = -1;
    select.setAttribute('aria-hidden', 'true');

    // A label click focuses the hidden element; hand it to the trigger.
    select.addEventListener('focus', () => trigger.focus());
    select.addEventListener('change', () => sync(select));
    trigger.addEventListener('click', () => { if (current?.select === select) close({ focus: true }); else open(select); });
    trigger.addEventListener('keydown', event => onKeydown(event, select));
    trigger.addEventListener('blur', event => { if (!list.contains(event.relatedTarget)) close(); });
    list.addEventListener('mousedown', event => event.preventDefault());
    list.addEventListener('mousemove', event => {
      const row = event.target.closest?.('[role="option"]');
      if (row && Number(row.dataset.index) !== current?.index) highlight(Number(row.dataset.index));
    });
    list.addEventListener('click', event => {
      const row = event.target.closest?.('[role="option"]');
      if (row) choose(Number(row.dataset.index));
    });
    new MutationObserver(() => sync(select)).observe(select, {
      subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ['disabled', 'selected', 'aria-label'],
    });
    sync(select);
  }

  function scan(root) {
    if (root.matches?.('select')) enhance(root);
    root.querySelectorAll?.('select').forEach(enhance);
  }
  scan(document);
  new MutationObserver(records => {
    for (const record of records) record.addedNodes.forEach(scan);
    if (current && !current.part.trigger.isConnected) close();
  }).observe(document.body, { subtree: true, childList: true });
  document.addEventListener('pointerdown', event => {
    if (current && !current.part.list.contains(event.target) && !current.part.trigger.contains(event.target)) close();
  }, true);
  window.addEventListener('resize', () => close());
  document.addEventListener('scroll', event => {
    if (current && !current.part.list.contains(event.target)) close();
  }, true);
  window.addEventListener('language-changed', () => document.querySelectorAll('select').forEach(sync));
})();
