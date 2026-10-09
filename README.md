# KVMFlow

按下 USB 切换器，同时切换键鼠和显示器。

KVMFlow 在本机监听共享 USB 设备的变化，通过 DDC/CI 向显示器发送输入源切换指令。
配合 USB Sharing Switch，可以在两台电脑间切换桌面，无需软件传输键鼠输入。

## 支持范围

- macOS Apple Silicon 和 Windows x64 桌面客户端。
- 一台或多台支持 DDC/CI 输入源切换的显示器。
- USB 切换器连接共享键鼠；显示器的视频输入分别连接两台电脑。
- 本地配置、托盘运行、开机启动、手动暂停和诊断导出。

日常 USB 检测和显示器切换在本机完成。自动检查更新会访问更新服务。

## 使用

1. 在两台电脑上安装 KVMFlow，并在显示器 OSD 中启用 DDC/CI。
2. 将键鼠接到 USB 切换器，将显示器的视频输入分别接到两台电脑。
3. 在客户端中配置 USB 触发设备和显示器，按引导记录本机与对端的输入值。
4. 保存配置，按下 USB 切换器，确认键鼠和显示器都已切换。

产品介绍见 [kvmflow.net](https://kvmflow.net)。
后续 GitHub 安装包将发布到 [Releases](https://github.com/Kerw1n1209/KVMFlow/releases)。

## 硬件限制

- 显示器、转接器、扩展坞和连接方式都会影响 DDC/CI 支持。
- 输入源编码存在厂商差异，需要逐台记录。
- 某些显示器仅在当前活动输入端响应 DDC/CI。软件在 USB 离开时尝试将画面切到对端。
- Windows 的 USB 设备移除通知可能晚于实际按键，切换延迟受系统和驱动影响。
- DDC 调用成功表示指令已发送，实际画面是否切换仍需确认。

出现问题时，先用显示器 OSD 恢复正确输入，再检查客户端配置。报告问题前请删除诊断中的
设备序列号、电脑名称、本地路径及其他个人信息。

## 源码布局

| 目录 | 内容 |
| --- | --- |
| `desktop/` | Tauri 2 客户端、本地界面、托盘和构建脚本 |
| `sidecar/` | Rust 切换状态机、USB/DDC 平台实现与模拟测试 |
| `probes/` | 硬件诊断工具、脱敏回归数据及 macOS 所需的 m1ddc 源码 |
| `shared/` | 共用品牌素材 |
| `docs/` | 本地构建和更新说明 |

本仓库发布桌面软件及其构建依赖。官网、管理后台、云端运营服务和内部记录独立维护。

## 开发

准备 Node.js LTS、Rust 和平台开发工具。macOS 需要 Xcode Command Line Tools；
Windows 需要 MSVC Rust 工具链、Visual Studio C++ Build Tools、Windows SDK 和 WebView2。

```sh
git clone https://github.com/Kerw1n1209/KVMFlow.git
cd KVMFlow
cargo test --manifest-path sidecar/Cargo.toml --workspace
cd desktop
npm ci
npm test
npm run smoke
npm start
```

开发和本地构建不需要官方更新签名私钥。macOS 构建会从仓库中的 m1ddc 源码准备 DDC 工具。

更多说明见 [构建指南](docs/building.md)、[更新说明](docs/auto-updates.md) 和
[贡献规范](CONTRIBUTING.md)。

## 许可证

KVMFlow 使用 [MIT License](LICENSE)。第三方代码保留其原始许可和归属，
见 [第三方声明](THIRD_PARTY_NOTICES.md)。
