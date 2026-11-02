// Appearance preference: follow the system, light or dark. This runs from
// <head> before the stylesheets paint, so a dark window never flashes light.
(() => {
  const storageKey = 'kvmflow-theme-v1';
  const preferences = ['system', 'light', 'dark'];
  const media = typeof window.matchMedia === 'function' ? window.matchMedia('(prefers-color-scheme: dark)') : null;
  let preference = 'system';
  try {
    const stored = localStorage.getItem(storageKey);
    if (preferences.includes(stored)) preference = stored;
  } catch { /* storage can be unavailable; follow the system */ }
  const resolved = () => preference === 'system' ? (media?.matches ? 'dark' : 'light') : preference;
  const apply = () => {
    const root = document.documentElement;
    root.dataset.theme = resolved();
    root.dataset.themePreference = preference;
  };
  const notify = () => window.dispatchEvent(new CustomEvent('theme-changed', { detail: { preference, theme: resolved() } }));
  function set(value) {
    if (!preferences.includes(value)) return;
    preference = value;
    try { localStorage.setItem(storageKey, value); } catch { /* the choice still applies to this window */ }
    apply(); notify();
  }
  media?.addEventListener?.('change', () => {
    if (preference !== 'system') return;
    apply(); notify();
  });
  apply();
  window.KVMFlowTheme = {
    set,
    preferences,
    get preference() { return preference; },
    get theme() { return resolved(); },
  };
})();
