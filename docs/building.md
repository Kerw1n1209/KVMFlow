# 从源码构建

## 环境

- Node.js LTS 和 Rust。
- macOS Apple Silicon：Xcode Command Line Tools。
- Windows x64：MSVC Rust 工具链、Visual Studio C++ Build Tools、Windows SDK。
- Windows 运行客户端需要 WebView2；NSIS 安装器可以处理其安装。

推荐在对应系统上构建。跨系统构建成功不能替代目标系统的安装与硬件验证。

## 开发与测试

在仓库根目录运行：

```sh
cargo test --manifest-path sidecar/Cargo.toml --workspace
cd desktop
npm ci
npm test
npm run test:rust
npm run smoke
npm start
```

Tauri 的准备脚本会同步品牌素材。macOS 上还会构建或复用 m1ddc，
源码已经包含在仓库中，无需访问官网后台或云端运营服务。

## 无更新签名的本地构建

官方发布脚本会要求更新签名私钥。本地测试不需要该私钥，可以通过 Tauri 配置
关闭更新包生成。下面的命令在 `desktop/` 中运行。

macOS：

```sh
npx tauri build --bundles app --config '{"bundle":{"createUpdaterArtifacts":false}}'
```

产物位于 `src-tauri/target/release/bundle/macos/KVMFlow.app`。

Windows PowerShell：

```powershell
$localConfig = Join-Path ([IO.Path]::GetTempPath()) (([guid]::NewGuid().ToString()) + '.json')
[IO.File]::WriteAllText($localConfig, '{"bundle":{"createUpdaterArtifacts":false}}', [Text.UTF8Encoding]::new($false))
try {
    npx tauri build --bundles nsis --config $localConfig
} finally {
    Remove-Item $localConfig
}
```

产物位于 `src-tauri/target/release/bundle/nsis/`。

关闭更新包生成只影响本次构建，应用内更新地址仍来自 `tauri.conf.json`。
自己发行派生版本时，应设置自己的更新验签公钥和更新地址。
这些本地产物没有获得官方发布签名；更新签名与系统代码签名、公证也是不同的步骤。

## 独立诊断服务

桌面客户端直接内嵌 Rust runtime，无需另启 sidecar 进程。
需要调试 JSONL 协议时，可以在根目录运行：

```sh
cargo build --release --manifest-path sidecar/Cargo.toml
```
