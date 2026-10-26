<div align="center">

<a href="https://kvmflow.net"><img src="shared/branding/logo.png" width="112" alt="KVMFlow logo: two connected monitors"></a>

# KVMFlow

### One button. Your keyboard, mouse, and monitors switch together.

**Press your USB sharing switch. KVMFlow switches the monitor inputs.**<br>
A local desktop app watches your shared USB devices and sends DDC/CI commands,<br>
so you can move between computers without separately reaching for each monitor’s menu.

[![Release](https://img.shields.io/github/v/release/Kerw1n1209/KVMFlow?label=release&color=111111)](https://github.com/Kerw1n1209/KVMFlow/releases) [![Stars](https://img.shields.io/github/stars/Kerw1n1209/KVMFlow?style=flat&color=111111)](https://github.com/Kerw1n1209/KVMFlow/stargazers) [![License](https://img.shields.io/badge/license-MIT-111111)](LICENSE)<br>
![macOS Apple Silicon](https://img.shields.io/badge/macOS-Apple_Silicon-111111?logo=apple&logoColor=white) ![Windows x64](https://img.shields.io/badge/Windows-x64-0078D4)

**[Website](https://kvmflow.net)** &nbsp;·&nbsp; **[Releases](https://github.com/Kerw1n1209/KVMFlow/releases)** &nbsp;·&nbsp; **[Build guide](docs/building.md)** &nbsp;·&nbsp; **[Report an issue](https://github.com/Kerw1n1209/KVMFlow/issues)** &nbsp;·&nbsp; **English** · [简体中文](README.zh-CN.md)

<br>

<table>
<tr>
<td align="center" width="25%"><h3>1 button</h3>keyboard, mouse, and displays</td>
<td align="center" width="25%"><h3>Mixed mode</h3>keep each monitor on one computer</td>
<td align="center" width="25%"><h3>LAN pairing</h3>set everything up from one computer</td>
<td align="center" width="25%"><h3>Local</h3>no cloud, no input forwarding</td>
</tr>
</table>

</div>

## Is KVMFlow for you?

KVMFlow is for a desk where multiple computers share a keyboard, mouse, and one or more monitors. A USB sharing switch moves the USB devices; each computer also has its own video connection to the monitors. KVMFlow brings those two actions together.

- **Keep your existing USB switch.** Identify the shared device group during setup.
- **Record each monitor’s inputs.** Map the raw DDC/CI input values to the connected computers.
- **Work locally.** USB detection and input switching run on each computer. Automatic update checks use the update service.
- **Stay in control.** Run from the tray, launch at login, pause automatic switching, and export diagnostics.
- **Mixed mode (beta).** Assign each monitor to one computer. The USB switch then moves only the keyboard and mouse while the pictures stay put; turning mixed mode off asks the other computer to switch its screens back.
- **LAN pairing (beta).** Both computers find each other on the local network and pair after you compare a 6-digit code. You then set everything up on the primary computer, without typing input values on the secondary.

You need monitors that support DDC/CI input switching. KVMFlow does not transmit keyboard or mouse input over the network; the LAN link only syncs configuration and coordinates switching.

## Quick start

**1 · Install on both computers.** See [kvmflow.net](https://kvmflow.net) for product information and [GitHub Releases](https://github.com/Kerw1n1209/KVMFlow/releases) for packages. Supported desktop platforms are macOS on Apple Silicon and Windows x64. The packages are not commercially code-signed yet; see [Install notes](#install-notes) for opening them the first time.

**2 · Connect the hardware.** Plug the shared keyboard and mouse into the USB sharing switch. Connect each computer’s video output to its own monitor input, then enable **DDC/CI** in the monitor’s on-screen menu.

**3 · Set up.** Open KVMFlow on both computers and choose **This is the primary** or **This is the secondary** in **Setup**. The primary identifies the USB switch, records its own monitor input values, then connects to the secondary over the LAN; once both screens show the same 6-digit code, the secondary’s monitors appear on the primary. If the secondary can’t read a monitor’s input value, switch that monitor to the secondary once with its own buttons. Without a LAN connection, you can still enter the other computer’s values by hand on the primary.

**4 · Press USB Switch.** Check that both the USB devices and the actual monitor picture move to the intended computer.

Change the interface language under **Settings → Language**: follow the system, 简体中文, or English. The preference also applies to the tray, notifications, and native confirmation dialogs.

### Mixed mode

Turn on mixed mode in **Settings** and assign each monitor to a computer. It takes effect right away: the USB switch moves only the keyboard and mouse, and each monitor stays on its computer. On macOS, monitors that belong to the other computer are temporarily detached so the cursor can’t wander onto a screen you can’t see. You can also toggle mixed mode from the tray. When you turn it off, KVMFlow asks the other computer over the LAN to switch its screens back, so both computers need to be online.

Display detach on Windows hasn’t been verified on real hardware yet and is off by default.

### Install notes

- **macOS**: if macOS says it can’t verify the developer, open **System Settings → Privacy & Security** and click **Open Anyway**. Allow **Local Network** access the first time you use LAN pairing.
- **Windows**: if SmartScreen appears, click **More info → Run anyway**. LAN pairing uses TCP port 47820; on a **Public** network, Windows Firewall may block the connection.
- **Beta builds**: turn on **Receive beta updates** in **Settings** to get beta versions through in-app updates.

## Screenshots

KVMFlow v0.2.7 (mixed mode and pairing screenshots coming), using example device data. Input values are examples, not presets for your monitors.

![KVMFlow v0.2.7 status page](docs/screenshots/v0.2.7/en/status.png)

[Setup, settings and diagnostics screenshots](docs/screenshots/README.md)

## How it works

```mermaid
flowchart LR
  Button["Press USB Switch"] --> USB["Shared keyboard and mouse move"]
  USB --> App["KVMFlow detects the USB change"]
  App --> DDC["DDC/CI input-switch commands"]
  DDC --> Monitors["Monitors select the other computer"]
```

The desktop client embeds the Rust runtime. Each monitor has its own input mapping, and the switch state machine handles timing, retries, and repeat protection. Simulated tests exercise the runtime without physical hardware.

The LAN channel discovers other KVMFlow computers with mDNS, encrypts connections with the Noise protocol, and confirms identity with a 6-digit code comparison; only paired computers can sync configuration or request a switch.

## Hardware notes

| Factor | What to check |
| :-- | :-- |
| **DDC/CI support** | Monitors, adapters, docks, and video connections can affect control support. Enable DDC/CI in the monitor menu. |
| **Input codes** | Codes vary by manufacturer. Record the raw values for each monitor. |
| **Inactive inputs** | Some monitors only respond on their currently active input. KVMFlow tries to send the away command when USB devices leave. |
| **Windows timing** | Device-removal notifications can arrive after the physical button press. Latency depends on the system and drivers. |
| **Picture confirmation** | A successful DDC call means the command was sent. Confirm the actual picture yourself. |

If switching fails, recover the correct input with the monitor menu first, then check KVMFlow’s mappings and diagnostics. Exported diagnostics replace device serial numbers, computer names, home folder paths, and LAN addresses with placeholders; still review the file for anything else personal before sharing it.

## Development

Prepare Node.js LTS, Rust, and the target platform’s development tools. macOS needs Xcode Command Line Tools. Windows needs the MSVC Rust toolchain, Visual Studio C++ Build Tools, Windows SDK, and WebView2.

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

Local development and builds do not need the official update-signing private key. macOS builds prepare the DDC helper from the m1ddc source included in this repository. Linux can run the core, simulated sidecar, and frontend tests; it is not a supported native hardware client.

| Directory | Contents |
| :-- | :-- |
| `desktop/` | Tauri 2 client, local interface, tray, and build scripts |
| `sidecar/` | Rust switch state machine, USB/DDC adapters, and simulated tests |
| `probes/` | Hardware diagnostics, sanitized fixtures, and m1ddc source |
| `shared/` | Canonical branding assets |
| `docs/` | Local build and update documentation |

Interface text and language defaults live in [`messages.json`](desktop/src/renderer/i18n/messages.json). The renderer and native host share this catalog. See the [localization guide](desktop/src/renderer/i18n/README.md) for configuration and testing.

## Documentation and feedback

- [Build guide](docs/building.md): platform prerequisites and local builds without update signing.
- [Automatic updates](docs/auto-updates.md): update verification and release configuration.
- [Contributing](CONTRIBUTING.md): contribution scope, commit conventions, and checks.
- [Issues](https://github.com/Kerw1n1209/KVMFlow/issues): include your OS, monitor and connection details, reproduction steps, and sanitized diagnostics.

This repository contains the desktop software and its build dependencies. The official website, administration tools, cloud operations, and internal records are maintained separately.

## License

[MIT](LICENSE). Third-party code retains its original licenses and attribution; see [Third-party notices](THIRD_PARTY_NOTICES.md).
