# 第三方声明

## m1ddc

macOS DDC 工具的源码来自 [waydabber/m1ddc](https://github.com/waydabber/m1ddc)，
版本 v1.2.0，提交前缀 `2549fec`。源码及原始 MIT 许可证位于
`probes/mac/experiments/m1ddc-stock/m1ddc-2549fec/`。

`probes/mac/Sources/m1ddcShim/` 的服务发现和 DDC/CI I2C 实现也移植自 m1ddc；
源码注释记录了 v1.2.0 和 master 的来源。相关代码适用以下许可：

```text
MIT License

Copyright (c) 2021 waydabber

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## 软件包依赖

Rust 和 Node.js 依赖由各目录的 manifest 和 lockfile 记录，适用各自的许可证。
重新分发软件时，应保留随依赖提供的版权和许可声明。

**完整传递依赖许可材料**（S9）：运行
`node tools/generate-third-party-licenses.js` 会从本地 cargo registry
缓存提取 sidecar workspace 锁定依赖图（含 `curve25519-dalek` 的
BSD-3-Clause、`memchr` 的 Unlicense/MIT、`fiat-crypto` 的多重许可等全部
传递依赖）中每个 crate 的许可证文件，生成 `third-party-licenses/`
目录（含 `manifest.json` 清单）。发布 macOS / Windows 安装包时应一并
附上该目录。`node tools/verify-lan-release.js --licenses` 在 CI 中校验
生成过程无缺失。

## 局域网通道（Rust，S4/S5）

下表版本由 `sidecar/Cargo.lock` 固定；上游随包提供 MIT / Apache-2.0
许可文本，分发时与其他 Rust 依赖一并保留。

| 依赖 | 版本 | SPDX 许可证 | 选择理由 |
| --- | --- | --- | --- |
| [snow](https://github.com/mcginty/snow) | 0.10.0-kvmflow.1（基于 0.10.0） | Apache-2.0 OR MIT | 同步、纯 Rust Noise XX；公开握手哈希与远端静态公钥，适合现有 std 线程模型。仅启用 X25519、ChaChaPoly、BLAKE2s 和系统随机源。 |
| [mdns-sd](https://github.com/keepsimple1/mdns-sd) | 0.21.6 | Apache-2.0 OR MIT | 自带线程与 channel 的 mDNS 发布、发现和接口变化检测，无需引入异步 runtime。 |
| [blake2](https://github.com/RustCrypto/hashes) | 0.10.6 | MIT OR Apache-2.0 | 配对数字的 BLAKE2s commit–reveal；复用 Noise 使用的 RustCrypto 实现。 |
| [sha2](https://github.com/RustCrypto/hashes) | 0.10.9 | MIT OR Apache-2.0 | mDNS 公钥和组 ID 的 SHA-256 短摘要，仅作发现提示。 |
| [getrandom](https://github.com/rust-random/getrandom) | 0.3.4 | MIT OR Apache-2.0 | 系统安全随机源，用于配对 nonce 和身份临时文件名。 |
| [zeroize](https://github.com/RustCrypto/utils) | 1.9.1 | Apache-2.0 OR MIT | 私钥和存储缓冲区在释放时清零，不实现 Debug 或 Serialize。 |
| [socket2](https://github.com/rust-lang/socket2) | 0.6.5 | MIT OR Apache-2.0 | 明确设置 IPv6-only，IPv4 / IPv6 同端口监听，避免平台 dual-stack 默认行为不同。 |
| [subtle](https://github.com/dalek-cryptography/subtle) | 2.6.1 | BSD-3-Clause | 对固定长度配对承诺和公钥使用常量时间比较。 |

Windows 密钥保护复用已有 `windows` 0.62 依赖（MIT OR Apache-2.0），
新增 Cryptography / Memory features 调用当前用户范围的 DPAPI；不请求管理员权限。

LAN 的 snow 现使用 `sidecar/vendor/snow` 中的 `0.10.0-kvmflow.1` 本地补丁，
保留原 MIT / Apache-2.0 许可证；上游文件摘要和补丁覆盖范围分别见该目录
`UPSTREAM.json` 与 `PATCHES.md`。补丁用于清零 resolver 之外的握手派生状态，
并与 LAN 算法白名单共同限制覆盖范围。完整传递依赖许可材料见上面的
生成流程（`curve25519-dalek` 的 BSD-3-Clause 等都在其中）。

snow 补丁测试还通过 `upstream-snow` 别名精确依赖未修改的 snow 0.10.0
（Apache-2.0 OR MIT），用于握手与传输密文的逐字节对照；仅用于开发测试，
不会链接进应用。许可文本与上述 snow 上游一致。
