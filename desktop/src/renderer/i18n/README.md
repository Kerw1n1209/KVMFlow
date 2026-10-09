# Localization / 国际化

`messages.json` is the single text catalog for the renderer and native Tauri host. Both languages must contain the same keys and interpolation parameters. Keep markup, runtime protocol identifiers, user-entered names, device labels, input values, and hardware evidence out of translations.

`messages.json` 是前端与 Tauri 原生宿主共用的文案配置。中英文应包含相同的文案键及插值参数；HTML、协议标识、用户填写的名称、设备标签、输入值和硬件证据不属于翻译文案。

- `defaultLanguage`: initial preference (`auto`, `zh-CN`, or `en`).
- `fallbackLocale`: fallback catalog for unsupported languages or missing keys (`en`).
- `supportedLocales`: available catalogs (`zh-CN`, `en`).
- `messages`: authored text grouped by locale. Parameters such as `{p0}`, `{name}`, and `{changes}` are interpolated literally, without interpreting HTML.

默认跟随系统语言；中文系统使用简体中文，英文系统使用英文，其他系统回退到英文。设置中的语言选择保存于原生用户数据目录的 `ui-preferences.json`，前端缓存 `kvmflow-language-v1` 用于首次加载。原生偏好优先，不会改变 USB/DDC 配置。

The language selector follows the system by default. Chinese system languages use Simplified Chinese; English and unsupported system languages use English. The native `ui-preferences.json` file is authoritative, with a renderer cache under `kvmflow-language-v1` for initial loading. Hardware configuration is unchanged. A language change rerenders the current UI and retains unsaved editor values.

Use `t('message.key', { p0: value })` for dynamic UI text. Mark static elements with `data-i18n="message.key"`; use `data-i18n-title`, `data-i18n-aria-label`, or `data-i18n-placeholder` for attributes. Native code uses `tr(&app, "message.key")`. Native messages are compiled from the same JSON catalog, so rebuild the native host after changing them.

动态文案使用 `t()`，静态文本及无障碍属性使用 `data-i18n` 系列属性。原生文案使用同一份 JSON 编译进程序，修改后需要重新构建原生宿主。用户数据通过文本节点或转义后的 HTML 显示，不能当作 HTML 插入。

Run `npm test` in `desktop/` for catalog parity, placeholder checks, language fallback/persistence, editor preservation, update status, and existing UI regressions. Run `npm run test:rust` on a machine with Tauri platform prerequisites to check native preference persistence and host integration.

在 `desktop/` 运行 `npm test` 验证文案键和参数一致、语言回退与保存、草稿保留、更新状态及界面回归；原生测试 `npm run test:rust` 需要对应的 Tauri 平台开发依赖。
