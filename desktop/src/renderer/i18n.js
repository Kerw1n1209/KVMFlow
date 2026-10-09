// Text configuration is shared with the native host. Never translate user data.
(() => {
  const storageKey = 'kvmflow-language-v1';
  let config;
  const renderedMessages = new Map();
  let preference = 'auto';
  let locale = 'en';
  const systemLocale = () => {
    const languages = navigator.languages?.length ? navigator.languages : [navigator.language];
    return languages.find(language => /^(zh|en)(-|$)/i.test(language || '')) || 'en';
  };
  const resolve = value => /^zh(-|$)/i.test(value || '') ? 'zh-CN' : 'en';
  const normalize = value => ['auto', 'zh-CN', 'en'].includes(value)
    ? value : config?.defaultLanguage || 'auto';
  const storedPreference = () => {
    try { return normalize(localStorage.getItem(storageKey)); }
    catch { return 'auto'; }
  };
  function t(id, params = {}) {
    const text = config?.messages[locale]?.[id] ?? config?.messages[config.fallbackLocale]?.[id] ?? id;
    // A callback keeps literal $&, quotes and markup in user values unchanged.
    const result = text.replace(/\{([a-zA-Z0-9_]+)\}/g, (token, name) =>
      Object.hasOwn(params, name) ? String(params[name]) : token);
    renderedMessages.set(result, { id, params: { ...params } });
    if (renderedMessages.size > 256) renderedMessages.delete(renderedMessages.keys().next().value);
    return result;
  }
  function apply(root = document) {
    root.querySelectorAll('[data-i18n]').forEach(element => {
      element.textContent = t(element.dataset.i18n);
    });
    for (const attribute of ['title', 'aria-label', 'placeholder']) {
      root.querySelectorAll(`[data-i18n-${attribute}]`).forEach(element => {
        element.setAttribute(attribute, t(element.getAttribute(`data-i18n-${attribute}`)));
      });
    }
    document.documentElement.lang = locale;
  }
  function setPreference(value, { persist = true, notify = true } = {}) {
    preference = normalize(value);
    locale = preference === 'auto' ? resolve(systemLocale()) : preference;
    if (persist) {
      try { localStorage.setItem(storageKey, preference); } catch { /* Native preferences remain authoritative. */ }
    }
    apply();
    if (notify) window.dispatchEvent(new CustomEvent('language-changed', { detail: { preference, locale } }));
  }
  const api = window.KVMFlowI18n = {
    t, apply, setPreference, systemLocale,
    describe: value => renderedMessages.get(value) || value,
    format: value => value && typeof value === 'object' && value.id ? t(value.id, value.params) : String(value ?? ''),
    errorMessage: value => {
      for (const catalog of Object.values(config?.messages || {})) {
        const entry = Object.entries(catalog).find(([, message]) => message === value);
        if (entry) return t(entry[0]);
      }
      return value;
    },
    get locale() { return locale; },
    get preference() { return preference; },
  };
  api.ready = (window.KVMFLOW_I18N_CONFIG
    ? Promise.resolve(window.KVMFLOW_I18N_CONFIG)
    : fetch('i18n/messages.json').then(response => {
      if (!response.ok) throw new Error(`Unable to load language configuration (${response.status})`);
      return response.json();
    })).then(value => {
      config = value;
      setPreference(storedPreference(), { persist: false, notify: false });
    });
  window.addEventListener('languagechange', () => {
    if (preference === 'auto') {
      setPreference('auto', { persist: false });
      void window.kvmflow?.language?.set('auto', systemLocale()).catch(console.error);
    }
  });
})();
