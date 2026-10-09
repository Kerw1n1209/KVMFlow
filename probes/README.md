# 硬件诊断工具

这些工具用于排查显示器 DDC/CI 和 USB 设备检测问题。macOS 工具使用 Swift、
IOKit 和 CoreGraphics；Windows 工具使用 PowerShell 和系统 API。

DDC 调用成功表示系统接受了指令，实际画面是否切换需要人工确认。
写入输入源会真实切换显示器，诊断前请准备通过显示器 OSD 恢复画面。

## macOS

安装 Xcode Command Line Tools 后：

```sh
cd probes/mac
./build.sh
./.build/release/kvmprobe selftest
./.build/release/kvmprobe display-probe --log ../logs/mac-session.jsonl
./.build/release/kvmprobe usb-snapshot -o ../logs/mac-usb.json --log ../logs/mac-session.jsonl
./.build/release/kvmprobe usb-watch --duration 120 --log ../logs/mac-session.jsonl
```

按工具列出的显示器 ID 读取输入源：

```sh
./.build/release/kvmprobe ddc-get <displayID> --vcp 60 --log ../logs/mac-session.jsonl
```

`experiments/m1ddc-stock/` 包含桌面客户端需要的 m1ddc 源码与构建脚本。
其他实验目录用于保留独立诊断实现。

## Windows

在仓库根目录的 PowerShell 中运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\probes\windows\DisplayProbe.ps1 -Mode probe -LogPath .\probes\logs\win-session.jsonl
powershell -NoProfile -ExecutionPolicy Bypass -File .\probes\windows\UsbProbe.ps1 -Mode snapshot -OutFile .\probes\logs\win-usb.json -LogPath .\probes\logs\win-session.jsonl
powershell -NoProfile -ExecutionPolicy Bypass -File .\probes\windows\UsbProbe.ps1 -Mode watch -DurationSec 120 -LogPath .\probes\logs\win-session.jsonl
```

脚本可使用 Windows PowerShell 5.1。运行模拟回归测试需要 PowerShell 7：

```powershell
pwsh -NoProfile -File .\probes\windows\tests\run-tests.ps1
```

## 回归数据与隐私

`tests/fixtures/usb-switch-bounce-20260913.jsonl` 来自一次 USB 接触抖动采集。
公开版本保留事件时序、VID/PID 和事件顺序，将设备序列号与系统实例路径替换为
稳定的虚构标识。Rust 回归测试使用该数据检查防抖和冷却行为。

运行工具生成的日志可能包含电脑名称、设备序列号、实例路径和连接信息。
这些原始日志不纳入仓库。向公开 Issue 提交诊断前，应先脱敏，并检查截图中的个人信息。

## 许可证

本项目代码使用根目录 MIT 许可证。移植的 m1ddc 代码保留原始归属和许可，
见根目录 `THIRD_PARTY_NOTICES.md` 及 vendored 源码中的 `LICENSE`。
