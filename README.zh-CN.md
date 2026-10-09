<div align="center">

<a href="https://kvmflow.net"><img src="shared/branding/logo.png" width="112" alt="KVMFlow Logo：两台相连的显示器"></a>

# KVMFlow

### 一个按钮，键鼠与显示器一起切换。

**按下 USB 切换器，KVMFlow 自动切换显示器输入源。**<br>
本地桌面应用监听共享 USB 设备，通过 DDC/CI 发送切换指令，<br>
在电脑之间切换时，无需再逐台操作显示器菜单。

[![Release](https://img.shields.io/github/v/release/Kerw1n1209/KVMFlow?label=release&color=111111)](https://github.com/Kerw1n1209/KVMFlow/releases) [![Stars](https://img.shields.io/github/stars/Kerw1n1209/KVMFlow?style=flat&color=111111)](https://github.com/Kerw1n1209/KVMFlow/stargazers) [![License](https://img.shields.io/badge/license-MIT-111111)](LICENSE)<br>
![macOS Apple Silicon](https://img.shields.io/badge/macOS-Apple_Silicon-111111?logo=apple&logoColor=white) ![Windows x64](https://img.shields.io/badge/Windows-x64-0078D4)

**[官网](https://kvmflow.net)** &nbsp;·&nbsp; **[下载与发行](https://github.com/Kerw1n1209/KVMFlow/releases)** &nbsp;·&nbsp; **[构建指南](docs/building.md)** &nbsp;·&nbsp; **[反馈问题](https://github.com/Kerw1n1209/KVMFlow/issues)** &nbsp;·&nbsp; [English](README.md) · **简体中文**

<br>

<table>
<tr>
<td align="center" width="25%"><h3>一个按钮</h3>键鼠与画面一起切换</td>
<td align="center" width="25%"><h3>多台显示器</h3>逐台记录输入映射</td>
<td align="center" width="25%"><h3>本地运行</h3>检测 USB 并发送切换指令</td>
<td align="center" width="25%"><h3>中文 / English</h3>跟随系统或手动选择</td>
</tr>
</table>

</div>

## KVMFlow 适合你吗？

如果你的桌面上有多台电脑，共用一套键鼠和一台或多台显示器，KVMFlow 可以把 USB 切换与显示器切换连起来：USB Sharing Switch 负责连接键鼠，各台电脑分别通过视频线连接显示器，KVMFlow 负责发送输入源切换指令。

- **继续使用现有 USB 切换器。** 在初始化时识别共享设备组。
- **逐台记录显示器输入值。** 将原始 DDC/CI 输入值对应到各台电脑。
- **在本机完成日常切换。** USB 检测和输入切换都在本地运行；自动检查更新会访问更新服务。
- **随时控制运行状态。** 支持托盘运行、开机启动、暂停自动切换和诊断导出。

显示器需要支持通过 DDC/CI 切换输入源。KVMFlow 不通过网络传输键鼠输入。

## 快速开始

**1 · 在两台电脑上安装。** 产品介绍见 [kvmflow.net](https://kvmflow.net)，安装包发布后可在 [GitHub Releases](https://github.com/Kerw1n1209/KVMFlow/releases) 获取。桌面客户端支持 macOS Apple Silicon 和 Windows x64。

**2 · 连接设备。** 将共用键鼠接到 USB Sharing Switch，各台电脑的视频输出分别连接显示器的不同输入口，并在显示器 OSD 菜单中启用 **DDC/CI**。

**3 · 记录输入映射。** 在各台电脑打开 KVMFlow，通过**初始化**识别 USB Switch，记住本机显示器读到的原始输入值，再填写另一台电脑的值，保存配置。

**4 · 按下 USB Switch。** 确认键鼠与显示器实际画面都切换到目标电脑。

在**设置 → 语言**选择跟随系统、简体中文或 English。语言偏好也会用于托盘、系统通知和原生确认弹窗。

## 如何工作

```mermaid
flowchart LR
  Button["按下 USB Switch"] --> USB["共享键鼠切换电脑"]
  USB --> App["KVMFlow 检测 USB 变化"]
  App --> DDC["发送 DDC/CI 输入切换指令"]
  DDC --> Monitors["显示器切到另一台电脑"]
```

桌面客户端直接内嵌 Rust runtime。各台显示器有独立的输入映射，切换状态机负责时序、重试和重复保护；模拟测试可以在没有真实硬件时验证 runtime。

## 硬件注意事项

| 因素 | 需要检查什么 |
| :-- | :-- |
| **DDC/CI 支持** | 显示器、转接器、扩展坞和视频连接方式都会影响控制能力，请在显示器菜单启用 DDC/CI。 |
| **输入编码** | 不同厂商的输入源编码有差异，需要逐台记录原始数值。 |
| **非当前输入** | 某些显示器只在活动输入端响应 DDC/CI；软件会在 USB 离开时尝试发送切到对端的指令。 |
| **Windows 时序** | USB 移除通知可能晚于实际按键，延迟取决于系统和驱动。 |
| **画面确认** | DDC 调用成功表示指令已发送，仍需确认显示器实际画面。 |

如果切换失败，先用显示器菜单恢复正确输入，再检查 KVMFlow 的映射与诊断。分享诊断文件前，请删除设备序列号、电脑名称、本地路径及其他个人信息。

## 开发

准备 Node.js LTS、Rust 和对应平台开发工具。macOS 需要 Xcode Command Line Tools；Windows 需要 MSVC Rust 工具链、Visual Studio C++ Build Tools、Windows SDK 和 WebView2。

```sh
git clone https://github.com/Kerw1n1209/KVMFlow.git
cd KVMFlow
cargo test --locked --manifest-path sidecar/Cargo.toml --workspace
cd desktop
npm ci
npm test
npm run smoke
npm run test:rust
npm start
```

开发与本地构建不需要官方更新签名私钥。macOS 构建会从仓库包含的 m1ddc 源码准备 DDC 工具。Linux 可运行核心、模拟 sidecar 和前端测试，但不属于原生硬件客户端的支持平台。

| 目录 | 内容 |
| :-- | :-- |
| `desktop/` | Tauri 2 客户端、本地界面、托盘和构建脚本 |
| `sidecar/` | Rust 切换状态机、USB/DDC 平台适配与模拟测试 |
| `probes/` | 硬件诊断、脱敏回归数据及 m1ddc 源码 |
| `shared/` | 统一品牌素材 |
| `docs/` | 本地构建和更新说明 |

界面文案与默认语言配置位于 [`messages.json`](desktop/src/renderer/i18n/messages.json)，前端和原生宿主共用此配置。维护方法与验证说明见[国际化配置指南](desktop/src/renderer/i18n/README.md)。

## 文档与反馈

- [构建指南](docs/building.md)：平台依赖及无需更新签名的本地构建。
- [更新说明](docs/auto-updates.md)：更新验签与发行配置。
- [贡献规范](CONTRIBUTING.md)：贡献范围、提交约定与检查。
- [Issues](https://github.com/Kerw1n1209/KVMFlow/issues)：请附系统、显示器和连接信息、复现步骤及脱敏诊断。

本仓库包含桌面软件及其构建依赖。官网、管理后台、云端运营服务和内部记录独立维护。

## 许可证

[MIT](LICENSE)。第三方代码保留原始许可和归属，见[第三方声明](THIRD_PARTY_NOTICES.md)。
