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
