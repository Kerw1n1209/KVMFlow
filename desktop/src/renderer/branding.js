// Generated from shared/branding/logo.png. Do not edit by hand.
(() => {
  window.KVMFLOW_BRAND = Object.freeze({"name":"KVMFlow","logoUrl":"assets/appIcon.png?v=0260e07fffd7"});
  document.documentElement.style.setProperty('--brand-logo-image', 'url("' + window.KVMFLOW_BRAND.logoUrl + '")');
  for (const rel of ['icon', 'apple-touch-icon']) {
    const link = document.createElement('link');
    link.rel = rel;
    link.type = 'image/png';
    link.href = window.KVMFLOW_BRAND.logoUrl;
    document.head.appendChild(link);
  }
})();
