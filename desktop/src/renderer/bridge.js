// The renderer has no filesystem, shell or arbitrary plugin access.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const listeners = new Map();
  const navigation = new Set();
  const updateListeners = new Set();
  let latestPage = null;
  const subscriptions = [
    listen('update-progress', ({ payload }) => {
      for (const cb of updateListeners) cb(payload);
    }),
    listen('runtime-notification', ({ payload }) => {
      for (const cb of listeners.get(payload.kind) || []) cb(payload.data);
      for (const cb of listeners.get('*') || []) cb(payload.kind, payload.data);
    }),
    listen('client-navigate', ({ payload }) => {
      latestPage = payload;
      for (const cb of navigation) cb(payload);
    }),
  ];
  const addListener = (kind, cb) => {
    if (!listeners.has(kind)) listeners.set(kind, new Set());
    listeners.get(kind).add(cb);
    return () => listeners.get(kind)?.delete(cb);
  };
  window.kvmflow = {
    ready: Promise.all(subscriptions),
    info: invoke('host_info'),
    clientReady: () => invoke('client_ready'),
    get initialPage() { return latestPage; },
    request: async (method, params = {}) => {
      try { return await invoke('runtime_request', { method, params }); }
      catch (error) {
        throw Object.assign(new Error(error?.message || String(error)), { code: error?.code });
      }
    },
    onNotification: addListener,
    onAllNotifications: (cb) => addListener('*', cb),
    onClientNavigate: (cb) => {
      navigation.add(cb);
      if (latestPage) cb(latestPage);
      return () => navigation.delete(cb);
    },
    exportDiagnostics: (payload) => invoke('export_diagnostics', { payload }),
    confirmDelete: (name) => invoke('confirm_delete', { name }),
    confirmLocalInput: (changes) => invoke('confirm_local_input', { changes }),
    updates: {
      check: () => invoke('update_check'),
      download: () => invoke('update_download'),
      install: () => invoke('update_install'),
      onProgress: (cb) => {
        updateListeners.add(cb);
        return () => updateListeners.delete(cb);
      },
    },
    startup: {
      get: () => invoke('startup_get'),
      set: (enabled) => invoke('startup_set', { enabled: enabled === true }),
    },
  };
  window.kvmflow.info.then((info) => {
    window.kvmflow.appVersion = info.version;
    if (info.platform === 'macos') {
      document.documentElement.classList.add('platform-darwin');
      const drag = document.createElement('div');
      drag.className = 'window-drag';
      drag.setAttribute('data-tauri-drag-region', '');
      document.body.prepend(drag);
    }
  });
})();
