// Every active logo derives from appIcon.png, including Icon Composer and tray.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { execFileSync } = require('node:child_process');
const desktop = path.resolve(__dirname, '..');
const root = path.dirname(desktop);
const canonical = path.join(root, 'shared/branding/logo.png');
const foreground = path.join(desktop, 'resources/branding/KVMFlow-AppIcon-Foreground.png');

function copy(source, destination) {
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  if (!fs.existsSync(destination) || !fs.readFileSync(source).equals(fs.readFileSync(destination))) {
    fs.copyFileSync(source, destination);
  }
}

function syncBranding({ nativeIcons = true } = {}) {
  copy(canonical, path.join(desktop, 'src/renderer/assets/appIcon.png'));
  if (fs.existsSync(path.join(root, 'website/package.json'))) {
    copy(canonical, path.join(root, 'website/public/kvmflow-icon.png'));
  }
  if (fs.existsSync(path.join(root, 'admin/package.json'))) {
    copy(canonical, path.join(root, 'admin/src/assets/kvmflow-icon.png'));
  }
  copy(canonical, foreground);
  copy(foreground, path.join(desktop, 'resources/branding/KVMFlow-AppIcon.icon/Assets/KVMFlow-AppIcon-Foreground.png'));
  for (const name of ['Source', 'LargeMark', 'Selected', 'Transparent']) {
    copy(canonical, path.join(desktop, `resources/branding/KVMFlow-AppIcon-${name}.png`));
  }
  if (fs.existsSync(path.join(root, 'artifacts/local-first-prototype'))) {
    copy(canonical, path.join(root, 'artifacts/local-first-prototype/kvmflow-app-icon.png'));
  }
  const hash = crypto.createHash('sha256').update(fs.readFileSync(canonical)).digest('hex').slice(0, 12);
  const brand = JSON.stringify({ name: 'KVMFlow', logoUrl: `assets/appIcon.png?v=${hash}` });
  fs.writeFileSync(path.join(desktop, 'src/renderer/branding.js'), `// Generated from shared/branding/logo.png. Do not edit by hand.\n(() => {\n  window.KVMFLOW_BRAND = Object.freeze(${brand});\n  document.documentElement.style.setProperty('--brand-logo-image', 'url("' + window.KVMFLOW_BRAND.logoUrl + '")');\n  for (const rel of ['icon', 'apple-touch-icon']) {\n    const link = document.createElement('link');\n    link.rel = rel;\n    link.type = 'image/png';\n    link.href = window.KVMFLOW_BRAND.logoUrl;\n    document.head.appendChild(link);\n  }\n})();\n`);
  if (process.platform !== 'darwin' || !nativeIcons) {
    console.log('Web branding synchronized from the shared artwork.');
    return;
  }
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-brand-export-'));
  try {
    execFileSync('swift', [path.join(__dirname, 'export-brand-icons.swift'), canonical, foreground, temporary], { stdio: 'inherit' });
    const iconset = path.join(temporary, 'KVMFlow.iconset');
    fs.mkdirSync(iconset);
    for (const size of [16, 32, 128, 256, 512]) {
      copy(path.join(temporary, `${size}.png`), path.join(iconset, `icon_${size}x${size}.png`));
      copy(path.join(temporary, `${size * 2}.png`), path.join(iconset, `icon_${size}x${size}@2x.png`));
    }
    execFileSync('iconutil', ['-c', 'icns', iconset, '-o', path.join(temporary, 'icon.icns')], { stdio: 'inherit' });
    copy(path.join(temporary, 'icon.icns'), path.join(desktop, 'build/icon.icns'));
    const sizes = [16, 24, 32, 48, 64, 128, 256];
    const header = Buffer.alloc(6 + sizes.length * 16);
    header.writeUInt16LE(1, 2);
    header.writeUInt16LE(sizes.length, 4);
    const images = sizes.map(size => fs.readFileSync(path.join(temporary, `${size}.png`)));
    let offset = header.length;
    sizes.forEach((size, i) => {
      const entry = 6 + i * 16;
      header[entry] = header[entry + 1] = size === 256 ? 0 : size;
      header.writeUInt16LE(1, entry + 4);
      header.writeUInt16LE(32, entry + 6);
      header.writeUInt32LE(images[i].length, entry + 8);
      header.writeUInt32LE(offset, entry + 12);
      offset += images[i].length;
    });
    fs.writeFileSync(path.join(desktop, 'build/icon.ico'), Buffer.concat([header, ...images]));
    copy(path.join(temporary, 'trayTemplate.png'), path.join(desktop, 'src/renderer/assets/trayTemplate.png'));
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
  console.log('Brand icons synchronized from the shared artwork.');
}
if (require.main === module) syncBranding({ nativeIcons: !process.argv.includes('--web-only') });
module.exports = { syncBranding };
