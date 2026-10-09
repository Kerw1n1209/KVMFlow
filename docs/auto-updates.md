# 应用更新

客户端使用 Tauri 2 updater。启动后检查更新一次，运行中每 6 小时检查一次，
也可以从设置页手动检查。

发现新版后，客户端显示版本与完整更新说明，设置页按钮变为「下载更新」。
更新说明默认展开。下载完成并通过签名验证后，按钮变为「重启并更新」，
可重启安装更新。未保存的设置需要先保存；安装失败时会恢复监听并保留更新包供重试。
检查更新失败不会阻止本地 USB/DDC 功能。

## 更新信任

官方更新验签公钥和 HTTPS 清单地址位于 `desktop/src-tauri/tauri.conf.json`。
公钥用于验证下载内容，可以公开；签名私钥不在仓库中。
客户端要求签名中的版本与更新清单一致。

自己发行派生版本时，应生成自己的签名密钥，并配置自己的验签公钥和更新地址。
不要将私钥或证书提交到仓库。自动发布应将签名材料保存在受保护的 CI secrets 中，
仅向可信的发布流程提供。

## 本地构建与发布构建

开发和无更新签名的本地构建见 [构建指南](building.md)。
`desktop/scripts/build-tauri.js` 用于准备签名发行包，会要求更新签名私钥。
可通过 `TAURI_SIGNING_PRIVATE_KEY` 或 `KVMFLOW_UPDATE_KEY_FILE` 指定自己的签名材料；
加密密钥口令使用 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。

脚本检查产物签名是否匹配客户端配置中的公钥。使用自己的密钥前，
必须同步更新自己发行版本的公钥配置。
更新签名独立于 Apple Developer ID、macOS 公证和 Windows Authenticode。

## 分发

GitHub Actions 自动构建 macOS arm64 和 Windows x64。只有两端构建、签名验证
和产物检查全部成功后，才发布 GitHub Release；失败不会发布半套安装包。
新客户端优先读取 GitHub Releases 的更新清单，原更新地址保留为备用。
已经安装且只认识原更新地址的旧客户端，需要先安装新包才能切换到新入口。

在 GitHub 的 Actions 中选择 `Build and release KVMFlow` → `Run workflow`，
选择 `main` 即可验证双平台构建并下载 `complete-release` 产物，不发布 Release。
正式发版时，先将 package.json、tauri.conf.json、Cargo.toml 的版本更新为同一版本，
并更新两份锁文件。将本次更新说明写入 `desktop/releases/<版本号>.md`，
同一份内容会用于 GitHub Release 和应用内的更新说明；缺失或为空时，发布流程会停止。
提交到 main，然后推送对应标签，例如：

```sh
git tag v0.2.5
git push origin v0.2.5
```

标签必须与代码版本一致，且指向 main 中的提交。已公开的同版本 Release 不会被覆盖。
仓库 Secret `TAURI_SIGNING_PRIVATE_KEY` 提供更新签名；密钥有口令时另设
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。工作流使用内置 GITHUB_TOKEN 发布，
无需配置额外 GitHub 访问令牌。

发布附件包含 DMG、Windows EXE、macOS 更新包、两份更新签名、latest.json
以及 SHA256SUMS.txt。此流程不自动同步 R2/OSS，也不修改官网或旧更新服务。

更新清单提供版本、说明、发布日期，以及各平台更新包的 URL 和签名。
当前发行目标为 macOS arm64 和 Windows x64。

## 验证

前端与 Rust 测试覆盖更新交互、未保存设置、失败恢复及签名版本检查。
正式发布前仍需在 macOS 和 Windows 真机上从旧版本升级，
确认安装、重启、配置保留、开机启动及 USB/DDC 行为。
