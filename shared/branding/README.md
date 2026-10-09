# 统一品牌素材

`logo.png` 是唯一手工维护的 Logo 源图，使用双显示器相连的统一品牌图案。英文与中文 README、桌面界面和打包图标均引用此源图或其派生素材。

- `brand.mjs` 提供共享品牌信息。
- 桌面页面通过生成的 `window.KVMFLOW_BRAND.logoUrl` 和 CSS 变量 `--brand-logo-image` 引用。
- `desktop/scripts/sync-branding.js` 同步打包资源，导出 ICNS、ICO 和菜单栏图标。派生文件不要单独修改。
- 各端构建会自动同步。手动同步：`node desktop/scripts/sync-branding.js`。

桌面应用携带本地素材副本，不依赖远程图片服务，支持离线使用。
