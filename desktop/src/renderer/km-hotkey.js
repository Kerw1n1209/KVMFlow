// Shortcuts in the engine's form, such as `Control+Alt+k`. Pure helpers shared
// by the setup page and settings.
(() => {
  const DEFAULT = 'Control+Alt+k';
  const HOME = 'Control+Alt+Escape';
  const NAMES = {
    macos: { Control: 'Control', Alt: 'Option', Shift: 'Shift', Super: 'Command' },
    windows: { Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Super: 'Win' },
  };
  const KEYS = { Escape: 'Esc', Space: 'Space', Tab: 'Tab' };
  // The engine would take these from every app on the computer. Listed for
  // both systems: the shortcut also runs while typing on the other computer.
  const RESERVED = new Set([
    ...[...'acvxzsqwnt'].flatMap(key => [`Control+${key}`, `Super+${key}`]),
    ...['h', 'm', 'l', 'd', 'e', 'r', 'Tab', 'Space'].map(key => `Super+${key}`),
    'Control+Tab', 'Control+Space', 'Control+Shift+Escape',
    'Alt+Tab', 'Alt+Space', 'Alt+F4',
    'Shift+Super+3', 'Shift+Super+4', 'Shift+Super+5',
  ]);

  function keyName(code) {
    let match = /^Key([A-Z])$/.exec(code);
    if (match) return match[1].toLowerCase();
    match = /^Digit(\d)$/.exec(code);
    if (match) return match[1];
    if (/^F([1-9]|1[0-2])$/.test(code) || KEYS[code]) return code;
    return null;
  }

  // A shortcut needs a modifier, or it would take the key away from typing.
  function fromEvent(event) {
    const key = keyName(event.code);
    const modifiers = [event.ctrlKey && 'Control', event.altKey && 'Alt', event.shiftKey && 'Shift', event.metaKey && 'Super'].filter(Boolean);
    return key && modifiers.length ? [...modifiers, key].join('+') : null;
  }

  function labels(hotkey, platform) {
    const names = NAMES[platform] || NAMES.macos;
    return String(hotkey || '').split('+').filter(Boolean).map(part => names[part] || KEYS[part] || part.toUpperCase());
  }

  // Captures the next shortcut. Esc alone cancels; a key without a modifier
  // keeps listening, as does a system shortcut or one `isTaken` refuses.
  // Returns a function that stops listening.
  function record(onDone, { isTaken = () => false, onTaken = () => {} } = {}) {
    const listen = event => {
      event.preventDefault();
      event.stopPropagation();
      const hotkey = fromEvent(event);
      if (!hotkey && event.code !== 'Escape') return;
      if (hotkey && (RESERVED.has(hotkey) || isTaken(hotkey))) { onTaken(); return; }
      stop();
      onDone(hotkey);
    };
    const stop = () => window.removeEventListener('keydown', listen, true);
    window.addEventListener('keydown', listen, true);
    return stop;
  }

  window.KVMFlowHotkey = { DEFAULT, HOME, fromEvent, labels, record };
})();
