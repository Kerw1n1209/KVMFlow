// Full-screen setup pages. Pure HTML builders: lan-pairing.js owns the state
// machine and passes plain view models; nothing here reads the runtime.
(() => {
  const t = (id, params) => window.KVMFlowI18n.t(id, params);
  const esc = value => String(value ?? '').replace(/[&<>"']/g, char => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[char]);

  const svg = (body, cls = '') => `<svg${cls ? ` class="${cls}"` : ''} viewBox="0 0 24 24" aria-hidden="true" focusable="false">${body}</svg>`;
  const icon = {
    arrow: svg('<path d="M5 12h14M13 6l6 6-6 6"/>', 'ob-arrow'),
    check: svg('<path d="M20 6 9 17l-5-5"/>'),
    chevron: svg('<path d="m6 9 6 6 6-6"/>'),
    close: svg('<path d="M6 6l12 12M18 6 6 18"/>'),
    monitor: svg('<rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/>', 'ob-ic'),
    laptop: svg('<rect x="4" y="5" width="16" height="11" rx="2"/><path d="M2 20h20"/>', 'ob-ic'),
    refresh: svg('<path d="M20 11a8 8 0 0 0-14.9-3.9M4 4v4h4M4 13a8 8 0 0 0 14.9 3.9M20 20v-4h-4"/>'),
  };

  // The product mark, drawn in place on the welcome and done pages. Both
  // screens keep their drawn colours; only the strokes and plate animate in.
  function mark(size) {
    return `<div class="ob-markwrap"><svg class="ob-mark" width="${size}" height="${size}" viewBox="4 4 108 108" aria-hidden="true" focusable="false">
<rect class="m-shadow" x="14" y="14" width="96" height="96" rx="20"/><rect class="m-plate" pathLength="1" x="8" y="8" width="96" height="96" rx="20"/>
<path class="m-c m-co m-c1" pathLength="1" d="M58 41H74a8 8 0 0 1 8 8V58"/><path class="m-c m-ci m-c1" pathLength="1" d="M58 41H74a8 8 0 0 1 8 8V58"/>
<rect class="m-a" x="19" y="26" width="41" height="28" rx="6"/><rect class="m-b" x="57.5" y="58.5" width="36" height="24" rx="5"/>
<path class="m-c m-co m-c2" pathLength="1" d="M35 55V63a8 8 0 0 0 8 8H71"/><path class="m-c m-ci m-c2" pathLength="1" d="M35 55V63a8 8 0 0 0 8 8H71"/><rect class="m-j" x="28" y="49" width="14" height="5"/></svg></div>`;
  }

  // Theme previews always show their own palette, whatever the current theme.
  const PALETTES = { light: ['#ededea', '#fff', '#000', '#d6d6d2'], dark: ['#161614', '#232320', '#f2f2ee', '#4a4a45'] };
  function preview(name) {
    const [b, s, i, m] = PALETTES[name];
    return `<svg viewBox="0 0 216 132" aria-hidden="true" focusable="false"><rect width="216" height="132" fill="${b}"/><rect width="58" height="132" fill="${s}"/><rect x="57" width="2" height="132" fill="${i}"/><rect x="10" y="12" width="14" height="14" rx="3" fill="${i}"/><rect x="29" y="16" width="20" height="5" fill="${i}"/><rect x="8" y="38" width="42" height="14" rx="2" fill="${m}" stroke="${i}" stroke-width="1.5"/><rect x="13" y="59" width="30" height="4" fill="${i}" opacity=".45"/><rect x="13" y="73" width="26" height="4" fill="${i}" opacity=".45"/><rect x="81" y="26" width="118" height="84" rx="3" fill="${i}"/><rect x="77" y="22" width="118" height="84" rx="3" fill="${s}" stroke="${i}" stroke-width="2"/><rect x="88" y="34" width="44" height="30" rx="2" fill="${i}"/><rect x="140" y="34" width="44" height="30" rx="2" fill="${m}" stroke="${i}" stroke-width="2"/><rect x="88" y="76" width="56" height="5" fill="${i}"/><rect x="88" y="87" width="38" height="4" fill="${i}" opacity=".45"/></svg>`;
  }

  const delay = seconds => `style="--d:${seconds}s"`;
  const head = (title, lead = '') => `<h1 class="ob-rise">${title}</h1>${lead ? `<p class="ob-lead ob-rise" ${delay(0.04)}>${lead}</p>` : ''}`;
  const mid = (inner, issues = '') => `<div class="ob-mid"><div class="ob-issues" data-region="issues">${issues}</div>${inner}</div>`;
  const foot = inner => `<div class="ob-foot">${inner}</div>`;
  const button = (action, label, { disabled = false, alt = false, small = false, arrow = false, stamp = null, attrs = '' } = {}) =>
    `<button type="button" class="ob-btn${alt ? ' alt' : ''}${small ? ' sm' : ''}${stamp != null ? ' ob-stamp' : ''}" ${stamp != null ? delay(stamp) : ''} data-pair-action="${action}" ${attrs} ${disabled ? 'disabled' : ''}>${label}${arrow ? ` ${icon.arrow}` : ''}</button>`;
  const ghost = (action, label, side, disabled = false) =>
    `<button type="button" class="ob-ghost ${side}" data-pair-action="${action}" ${disabled ? 'disabled' : ''}>${label}</button>`;
  const back = (action, disabled = false) => ghost(action, `← ${esc(t('ob.back'))}`, 'l', disabled);
  const scan = '<span class="ob-scan" aria-hidden="true"><i></i><i></i><i></i></span>';
  const box = (on = false) => `<span class="ob-box${on ? ' on' : ''}" aria-hidden="true">${icon.check}</span>`;
  const status = (done, label) => `<div class="ob-status" role="status">${done ? box(true) : scan}<span>${label}</span></div>`;
  function help() {
    const hint = esc(t('ob.direct.hint'));
    return `<button type="button" class="ob-help" aria-label="${hint}">?<span class="ob-tip" aria-hidden="true">${hint}</span></button>`;
  }
  function alert(title, copy) {
    return `<div class="ob-alert ob-stamp" role="alert"><b>${title}</b>${copy ? `<span>${copy}</span>` : ''}</div>`;
  }
  // Two computers joined by a cable: the pairing and test pages.
  function link({ selfPlatform, peer, peerPlatform, selfOn = true, peerOn = false, cable = 'run' }) {
    const card = (on, name, sub) => `<div class="ob-pc${on ? ' on' : ''}">${icon.laptop}<b>${name}</b>${sub ? `<small>${sub}</small>` : ''}</div>`;
    return `<div class="ob-link ob-rise">${card(selfOn, esc(t('this.computer')), esc(selfPlatform))}<div class="ob-cable ${cable}" aria-hidden="true"></div>${card(peerOn, esc(peer), esc(peerPlatform))}</div>`;
  }
  const monitorRows = rows => `<div class="ob-list">${rows.map((row, i) => `<div class="ob-card ob-row ob-stamp" ${delay(0.06 + i * 0.08)}>${icon.monitor}<div class="ob-grow"><b>${esc(row.name)}</b>${row.copy ? `<small>${row.copy}</small>` : ''}</div><span class="ob-tag${row.bad ? ' bad' : ''}">${row.tag}</span></div>`).join('')}</div>`;

  const pages = {
    welcome: ({ version }) => mid(`${mark(132)}<h1 class="ob-word" aria-label="KVMFlow">${[...'KVMFlow'].map((char, i) => `<span aria-hidden="true" style="--d:${(1.26 + i * 0.035).toFixed(3)}s">${char}</span>`).join('')}</h1><p class="ob-lead ob-rise" ${delay(1.72)}>${esc(t('ob.tagline'))}</p>${version ? `<div class="ob-ver ob-rise" ${delay(1.8)}>v${esc(version)}</div>` : ''}`)
      + foot(button('ob-theme', esc(t('v2.start')), { arrow: true, stamp: 1.95 })),

    theme: ({ preference, issues }) => mid(`${head(esc(t('ob.theme.title')), esc(t('ob.theme.copy')))}<div class="ob-themes" role="radiogroup" aria-label="${esc(t('appearance.title'))}">${[['system', 'appearance.system'], ['light', 'appearance.light'], ['dark', 'appearance.dark']].map(([name, label], i) => `<button type="button" class="ob-tc ob-stamp${preference === name ? ' on' : ''}" ${delay(0.06 + i * 0.05)} role="radio" aria-checked="${preference === name}" data-ob-theme="${name}"><span class="ob-pv">${name === 'system' ? preview('light') + preview('dark') : preview(name)}</span><span class="ob-lb">${esc(t(label))}${box(preference === name)}</span></button>`).join('')}</div>`, issues)
      + foot(back('ob-welcome') + button('start', esc(t('v2.next')), { arrow: true, stamp: 0.2 })),

    local: ({ reading, rows, busy, canNext, issues }) => {
      let list;
      if (reading) list = `<div class="ob-card ob-row ob-wait ob-stamp" ${delay(0.06)}>${scan}<span>${esc(t('ob.local.reading'))}</span></div>`;
      else if (!rows.length) list = `<div class="ob-card ob-row ob-stamp" ${delay(0.06)}>${icon.monitor}<div class="ob-grow"><div class="ob-namel"><b>${esc(t('ob.local.empty'))}</b>${help()}</div></div></div>`;
      else list = rows.map((row, i) => `<div class="ob-card ob-row ob-stamp" ${delay(0.06 + i * 0.08)} data-ob-row="${esc(row.fingerprint)}">${icon.monitor}<div class="ob-grow"><div class="ob-namel"><b>${esc(row.name)}</b>${row.ready ? '' : help()}</div>${row.ready ? '' : `<small>${esc(t('v2.ddc'))}</small>`}</div>${row.ready ? `<span class="ob-tag">${esc(t('ob.monitor.ready'))}</span>` : `${button('read-skip', esc(t('v2.read.skip')), { alt: true, small: true, disabled: busy, attrs: `data-fp="${esc(row.fingerprint)}"` })}<span class="ob-tag bad">${esc(t('v2.read.tag'))}</span>`}</div>`).join('');
      const retry = !reading && (!rows.length || rows.some(row => !row.ready));
      return mid(`${head(esc(t('ob.local.title')), esc(t('v2.start.copy')))}<div class="ob-list">${list}</div>`, issues)
        + foot(back('ob-back-theme') + button('local-next', esc(t('v2.next')), { arrow: true, disabled: !canNext }) + (retry ? ghost('read-retry', esc(t('detect.again')), 'r', busy) : ''));
    },

    connect: ({ rows, selfIp, ipOpen, ipError, manual, busy, offline, issues }) => {
      const devices = rows.length ? rows.map(row => `<div class="ob-row ob-stamp">${icon.laptop}<div class="ob-grow"><b>${esc(row.name)}</b>${row.detail ? `<small>${esc(row.detail)}</small>` : ''}</div>${button('v2-connect', esc(t('ob.pair')), { small: true, disabled: busy, attrs: `data-v2-peer="${esc(row.deviceId)}"` })}</div>`).join('')
        : `<div class="ob-empty">${esc(t(offline ? 'lan.list.empty.offline' : 'ob.searching'))}</div>`;
      const steps = ['v2.wait.usb', 'v2.wait.display', 'v2.wait.open'].map((id, i) => `<li><i>${i + 1}</i><span>${esc(t(id))}</span></li>`).join('');
      return mid(`${head(esc(t('v2.connect.title')))}<div class="ob-cols">
<div class="ob-card ob-near ob-stamp" ${delay(0.05)}><div class="ob-ch" data-region="near-head"><span>${esc(t('ob.nearby'))}</span>${offline ? '' : scan}</div><div class="ob-devs" data-region="devices" aria-live="polite">${devices}</div>
<div class="ob-ipwrap${ipOpen ? ' open' : ''}"><button type="button" class="ob-iptog" data-ob-ip aria-expanded="${ipOpen}" aria-controls="ob-ipbody"><span>${esc(t('v2.ip.connect'))}</span>${icon.chevron}</button>
<div class="ob-ipbody" id="ob-ipbody" ${ipOpen ? '' : 'inert'}><div><form class="ob-ipform v2-ip-form" novalidate><input id="ob-ip-addr" name="addr" aria-label="${esc(t('lan.ip.address'))}" aria-describedby="ob-iperr" placeholder="192.168.1.23" autocomplete="off" spellcheck="false" maxlength="21"><button type="submit" class="ob-btn alt" ${busy ? 'disabled' : ''}>${esc(t('lan.connect'))}</button></form><div class="ob-iperr" id="ob-iperr" data-region="ip-error" role="alert">${ipError ? esc(t(ipError)) : ''}</div></div></div></div></div>
<div class="ob-side ob-stamp" ${delay(0.11)}><h3>${esc(t('ob.notyet'))}</h3><ol>${steps}</ol><div class="ob-ip"><small>${esc(t('v2.local.ip'))}</small><code data-region="self-ip">${esc(selfIp || '—')}</code></div></div></div>`, issues)
        + foot(`<label class="ob-chk"><input type="checkbox" data-ob-manual ${manual ? 'checked' : ''}><span>${esc(t('ob.manual'))}</span></label>${ghost('ob-exit', esc(t('v2.skip')), 'r')}`);
    },

    pair: ({ peer, peerPlatform, selfPlatform, state, sas, waitingAccept = false, notes = [], pendingRows = [], error, busy, skippable = false, issues }) => {
      const skip = skippable ? ghost('ob-exit', esc(t('v2.skip')), 'r') : '';
      const title = head(esc(t('ob.pair.title', { p0: peer })));
      if (state === 'sas') {
        const digits = [...String(sas || '').replace(/\D/g, '')].map((digit, i) => `<span class="ob-stamp" ${delay(i * 0.04)}>${digit}</span>`).join('');
        return mid(`${title}<div class="ob-sas" aria-label="${esc(sas || '')}">${digits}</div><p class="ob-lead ob-rise" ${delay(0.28)}>${esc(t('ob.sas.question'))}</p>`, issues)
          + foot(back('v2-cancel') + button('mismatch', esc(t('ob.sas.no')), { alt: true, disabled: busy }) + button('confirm', esc(t('ob.sas.yes')), { disabled: busy }) + skip);
      }
      const failed = state === 'fail', ok = state === 'ok';
      const body = link({ selfPlatform, peer, peerPlatform, peerOn: ok, cable: ok ? 'ok' : failed ? 'stop' : 'run' });
      let below;
      if (failed) below = alert(error.title, error.copy);
      else if (!ok) below = status(false, esc(t(waitingAccept ? 'ob.pair.waiting.accept' : 'ob.pairing', { p0: peer })));
      else {
        below = status(true, esc(t('ob.paired')));
        below += notes.map(copy => `<p class="ob-note" role="status">${copy}</p>`).join('');
        if (pendingRows.length) below += monitorRows(pendingRows);
      }
      return mid(title + body + below, issues)
        + foot(ok ? '' : back('v2-cancel') + (failed ? button('v2-retry', esc(t('retry')), { disabled: busy }) : '') + skip);
    },

    test: ({ peer, peerPlatform, selfPlatform, state, hint, failedRows = [], failedNames, busy, issues }) => {
      if (state === 'error') {
        return mid(`${head(esc(t('v2.test.error')), esc(t('v2.test.error.copy', { p0: failedNames })))}${monitorRows(failedRows)}`, issues)
          + foot(ghost('feedback', esc(t('v2.feedback')), 'l') + button('test-retry', esc(t('retry')), { disabled: busy }));
      }
      const ok = state === 'ok';
      return mid(`${head(esc(t('v2.test.title')), esc(t('v2.test.copy', { p0: peer })))}${link({ selfPlatform, peer, peerPlatform, selfOn: !ok, peerOn: ok, cable: ok ? 'ok' : 'run' })}${status(ok, esc(t(ok ? 'ob.switched' : 'v2.test.wait')))}${hint && !ok ? `<p class="ob-note">${esc(t('v2.test.hint', { p0: peer }))}</p>` : ''}`, issues)
        + foot(ok ? '' : ghost('v2-skip', esc(t('v2.skip')), 'r', busy));
    },

    done: ({ issues }) => mid(`${mark(96)}<h1 class="ob-rise ob-done-title" ${delay(1.3)}>${esc(t('v2.done.title'))}</h1>`, issues)
      + foot(button('v2-finish', esc(t('v2.done')), { stamp: 1.45 })),

    error: ({ title, copy, busy, skippable = false, issues }) => mid(head(title, copy), issues)
      + foot(ghost('feedback', esc(t('v2.feedback')), 'l') + button('v2-retry', esc(t('retry')), { disabled: busy }) + (skippable ? ghost('ob-exit', esc(t('v2.skip')), 'r') : '')),
  };

  // The "add a computer" page inside the app window. Same actions and form
  // classes as the connect page, laid out with the app's panels.
  function add({ state, rows = [], selfName, selfIp, ipError, manual, busy, offline, refreshing, error, issues = '' }) {
    if (state === 'local') {
      return `${issues}<section class="panel add-state"><h2>${esc(t('add.local.title'))}</h2><p class="sub">${esc(t('add.local.copy'))}</p>
<button type="button" class="primary" data-pair-action="add-local" ${busy ? 'disabled' : ''}>${esc(t('add.local.action'))}</button></section>`;
    }
    if (state === 'error') {
      return `${issues}<section class="panel add-state" role="alert"><h2>${error.title}</h2><p class="sub">${error.copy}</p>
<div class="add-state-actions"><button type="button" class="secondary" data-pair-action="feedback">${esc(t('v2.feedback'))}</button><button type="button" class="primary" data-pair-action="add-retry" ${busy ? 'disabled' : ''}>${esc(t('retry'))}</button></div></section>`;
    }
    const loading = state !== 'ready';
    const devices = rows.length ? rows.map(row => `<div class="add-row">${icon.laptop}<div class="add-copy"><strong>${esc(row.name)}</strong>${row.detail ? `<span class="hint">${esc(row.detail)}</span>` : ''}</div><button type="button" class="primary" data-pair-action="v2-connect" data-v2-peer="${esc(row.deviceId)}" ${busy ? 'disabled' : ''}>${esc(t('lan.connect'))}</button></div>`).join('')
      : `<div class="add-empty">${offline ? '' : scan}<div class="add-copy"><strong>${esc(t(offline ? 'lan.list.empty.offline' : loading ? 'add.loading' : 'ob.searching'))}</strong>${offline ? '' : `<span class="hint">${esc(t('add.empty.hint'))}</span>`}</div></div>`;
    return `<div data-region="issues">${issues}</div><div class="add-grid">
<section class="panel add-near" aria-labelledby="add-near-title"><div class="add-head"><h2 id="add-near-title">${esc(t('ob.nearby'))}<span class="add-count" data-region="add-count">${rows.length || ''}</span></h2>
<button type="button" class="secondary add-refresh${refreshing ? ' spin' : ''}" data-pair-action="add-refresh" aria-label="${esc(t('add.refresh'))}" title="${esc(t('add.refresh'))}" ${loading || offline ? 'disabled' : ''}>${icon.refresh}</button></div>
<div class="add-list" data-region="devices" aria-live="polite">${devices}</div>
<form class="add-ip v2-ip-form" novalidate><label for="add-ip-addr">${esc(t('v2.ip.connect'))}</label><div class="add-ip-row"><input id="add-ip-addr" name="addr" aria-describedby="add-iperr" placeholder="192.168.1.23" autocomplete="off" spellcheck="false" maxlength="21"><button type="submit" class="secondary" ${busy || loading ? 'disabled' : ''}>${esc(t('lan.connect'))}</button></div><p class="add-iperr" id="add-iperr" data-region="ip-error" role="alert">${ipError ? esc(t(ipError)) : ''}</p></form></section>
<aside class="panel add-self" aria-labelledby="add-self-title"><h2 id="add-self-title">${esc(t('add.self'))}</h2><dl><dt>${esc(t('add.self.name'))}</dt><dd>${esc(selfName || '—')}</dd><dt>${esc(t('v2.local.ip'))}</dt><dd><code data-region="self-ip">${esc(selfIp || '—')}</code></dd></dl>
<p class="hint">${esc(t('add.self.copy'))}</p><label class="add-manual"><input type="checkbox" data-ob-manual ${manual ? 'checked' : ''}><span>${esc(t('ob.manual'))}</span></label></aside></div>`;
  }

  // Page indicator: theme, this computer, connect (and pair), test.
  const STEPS = { theme: 0, local: 1, connect: 2, pair: 2, error: 2, test: 3, done: 4 };
  function top({ page, locale, exit, dots: showDots = true }) {
    const step = showDots ? STEPS[page] : null;
    const dots = step == null ? '' : [0, 1, 2, 3].map(i => `<i class="${i <= step ? 'on' : ''}"></i>`).join('');
    const lang = [['zh-CN', '中'], ['en', 'EN']].map(([value, label]) => `<button type="button" lang="${value}" data-ob-lang="${value}" aria-pressed="${locale === value}" class="${locale === value ? 'on' : ''}">${label}</button>`).join('');
    return `<div class="ob-top"><div class="ob-pg" aria-hidden="true">${dots}</div><div class="ob-tools"><div class="ob-lang" role="group" aria-label="${esc(t('language.title'))}">${lang}</div>${exit ? `<button type="button" class="ob-close" data-pair-action="ob-exit" aria-label="${esc(t('ob.exit'))}" title="${esc(t('ob.exit'))}">${icon.close}</button>` : ''}</div></div>`;
  }

  window.KVMFlowOnboardingView = { pages, add, top, mark, esc };
})();
