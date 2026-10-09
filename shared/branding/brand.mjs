// One source of truth; Vite resolves the shared file into each deployment.
import logoUrl from './logo.png?url';

export const BRAND = Object.freeze({
  name: 'KVMFlow',
  logoUrl,
});

export function applyBranding(doc = document) {
  window.KVMFLOW_BRAND = BRAND;
  doc.documentElement.style.setProperty('--brand-logo-image', `url("${BRAND.logoUrl}")`);
  for (const rel of ['icon', 'apple-touch-icon']) {
    const link = doc.head.querySelector(`link[rel="${rel}"]`) || doc.createElement('link');
    link.rel = rel;
    link.type = 'image/png';
    link.href = BRAND.logoUrl;
    if (rel === 'icon') link.sizes = '1024x1024';
    if (!link.isConnected) doc.head.appendChild(link);
  }
}
