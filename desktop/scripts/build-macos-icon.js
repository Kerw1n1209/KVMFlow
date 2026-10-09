const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const desktopRoot = path.join(__dirname, '..');
const iconName = 'KVMFlow-AppIcon';
const iconDocument = path.join(desktopRoot, 'resources', 'branding', `${iconName}.icon`);
const outputDir = path.join(desktopRoot, 'build', 'macos-icon');

function buildMacOSIcon() {
  if (process.platform !== 'darwin') return;
  if (!fs.existsSync(iconDocument)) {
    throw new Error(`Missing Icon Composer document: ${iconDocument}`);
  }

  const temporaryDir = fs.mkdtempSync(path.join(os.tmpdir(), 'kvmflow-macos-icon-'));
  const partialInfo = path.join(temporaryDir, 'partial-info.plist');
  try {
    execFileSync('xcrun', [
      'actool',
      '--compile', temporaryDir,
      '--platform', 'macosx',
      '--minimum-deployment-target', '13.0',
      '--app-icon', iconName,
      '--output-partial-info-plist', partialInfo,
      '--warnings',
      '--errors',
      '--notices',
      '--output-format', 'human-readable-text',
      iconDocument,
    ], { stdio: 'inherit' });

    fs.mkdirSync(outputDir, { recursive: true });
    fs.copyFileSync(path.join(temporaryDir, 'Assets.car'), path.join(outputDir, 'Assets.car'));
  } finally {
    fs.rmSync(temporaryDir, { recursive: true, force: true });
  }
}

if (require.main === module) buildMacOSIcon();

module.exports = { buildMacOSIcon, iconName };
