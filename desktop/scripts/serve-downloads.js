#!/usr/bin/env node

// A deliberately small LAN-only release page. It exposes only the two
// named installers produced by this project, never an arbitrary directory.

const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');

const dist = path.join(__dirname, '..', 'dist');
const version = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'))).version;
const portArgument = process.argv.indexOf('--port');
const port = Number(portArgument >= 0 ? process.argv[portArgument + 1] : process.env.KVMFLOW_LAN_PORT || 4100);

if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('port must be an integer from 1 to 65535');
}

const artifacts = [
  {
    id: 'mac',
    filenames: [`KVMFlow-${version}-arm64.dmg`],
    platform: 'macOS（Apple Silicon）',
    instruction: '打开 DMG，将 KVMFlow 拖入“应用程序”文件夹。',
  },
  {
    id: 'windows',
    filenames: [`KVMFlow Setup ${version}.exe`],
    platform: 'Windows 10/11（x64）',
    instruction: '下载后双击运行安装程序。',
  },
];

function availableArtifacts() {
  return artifacts.map((artifact) => {
    const filename = artifact.filenames.find((candidate) => fs.existsSync(path.join(dist, candidate))) || artifact.filenames[0];
    const filePath = path.join(dist, filename);
    return {
      ...artifact,
      filename,
      filePath,
      available: fs.existsSync(filePath),
    };
  });
}

function addresses() {
  const addresses = new Set();
  for (const entries of Object.values(os.networkInterfaces())) {
    for (const entry of entries || []) {
      if (entry.family === 'IPv4' && !entry.internal) addresses.add(entry.address);
    }
  }
  return [...addresses].map((address) => `http://${address}:${port}/`);
}

function renderIndex(items) {
  const cards = items.map((item) => `<article class="artifact ${item.available ? '' : 'unavailable'}">
    <h2>${item.platform}</h2>
    <p>${item.instruction}</p>
    ${item.available
      ? `<a href="/download/${encodeURIComponent(item.id)}">下载 ${item.filename}</a>`
      : '<span>安装包尚未生成</span>'}
  </article>`).join('');
  return `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>KVMFlow 局域网下载</title><style>
    :root{font-family:-apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei",sans-serif;color:#171814;background:#eceae3}*{box-sizing:border-box}body{max-width:760px;margin:0 auto;padding:56px 28px;background:#eceae3}h1{margin:0;font-size:30px;letter-spacing:-.5px}p{color:#5f605a;line-height:1.6}.rule{height:1px;margin:22px 0 28px;background:#777870}.artifact{margin:14px 0;padding:24px;border:1px solid #777870;background:#f8f7f1}.artifact h2{margin:0;font-size:18px}.artifact p{margin:10px 0 18px;font-size:14px}.artifact a{display:inline-block;padding:9px 13px;background:#171814;color:#f8f7f1;text-decoration:none;font-weight:650}.artifact.unavailable{opacity:.58}.artifact span{font-size:13px;color:#5f605a}.note{margin-top:28px;font-size:13px}</style><main><h1>KVMFlow 下载</h1><p>选择这台电脑需要的安装包。</p><div class="rule"></div>${cards}<p class="note">此页面仅在当前局域网内可访问。下载完成后可关闭终端中的下载服务。</p></main></html>`;
}

const server = http.createServer((request, response) => {
  const url = new URL(request.url || '/', `http://${request.headers.host || 'localhost'}`);
  const items = availableArtifacts();
  if (url.pathname === '/') {
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
    response.end(renderIndex(items));
    return;
  }
  const id = url.pathname.startsWith('/download/') ? decodeURIComponent(url.pathname.slice('/download/'.length)) : null;
  const artifact = items.find((item) => item.id === id && item.available);
  if (!artifact) {
    response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
    response.end('未找到安装包。');
    return;
  }
  const stat = fs.statSync(artifact.filePath);
  response.writeHead(200, {
    'content-type': 'application/octet-stream',
    'content-length': stat.size,
    'content-disposition': `attachment; filename*=UTF-8''${encodeURIComponent(artifact.filename)}`,
    'cache-control': 'no-store',
  });
  fs.createReadStream(artifact.filePath).pipe(response);
});

server.listen(port, '0.0.0.0', () => {
  console.log('KVMFlow 局域网下载服务已启动：');
  const urls = addresses();
  if (urls.length) urls.forEach((url) => console.log(`  ${url}`));
  else console.log(`  http://127.0.0.1:${port}/`);
  console.log('按 Ctrl+C 停止服务。');
});
