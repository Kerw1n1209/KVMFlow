# m1ddc v1.2.0 原源码构建实验（产品级决定实验，非诊断）

> **结果（2026-09-13 11:41Z 真机执行，machine_observed）**：判定表第一行成立——
> G52plus 判定格（仍在 Mac 输入）自建版与 brew bottle **同读 7**；G73（已切离
> Mac）两者同读 0，一致性佐证。**MVP Mac DDC 路径定案：sidecar 调用自建
> m1ddc v1.2.0 子进程**（vendored 源 SHA 固定、MIT 归属保留、adhoc 签名与
> bottle 同类）。产品化接口：kvmprobe 0.1.12 起 `stock-ddc get|set` 包装命令
> （子进程命令行/退出码/stdout 全量入会话 JSONL，stdout 原样透传）。
>
> **封箱措辞修正（2026-09-13）**：自建版工作 → 下表第二行「bottle 与其自身
> 源码存在未知构建期差异」假说**不成立**——tag 源码在本机工具链下即可复现
> 工作行为。此前各复刻变体（shim 四格 / 纯 C / ObjC-main）的失败差异位于
> 未被逐字覆盖的 `m1ddc.m` 主流程细节中；产品上已无需追查（工作路径已定），
> 诊断封箱维持。二进制 SHA 随工具链而变（开发机 `b64a75d8…`、家庭 Mac
> `8c2c0d08…`，行为均正常）——**溯源锚点是 vendored 源码 SHA，不是二进制**。

> **归属**：`m1ddc-2549fec/` 目录是 [waydabber/m1ddc](https://github.com/waydabber/m1ddc)
> **v1.2.0 tag（commit `2549fec9f0bfe01a51f2ba1ee558d20933273e10`）** 的原样
> vendor——`sources/`（m1ddc.m / ioregistry.m / i2c.m）、`headers/`、`Makefile`、
> `LICENSE`（MIT, (c) 2021 waydabber）全部未修改，SHA-256 见交付评论。用其自带
> Makefile 构建（`-Wall -Werror -Wextra -fmodules`，链接 `-framework CoreDisplay`，
> 其余框架由 `@import` 拉入）。

## 目的（产品决定，不受诊断封箱约束）

KVM-1 真机上：brew bottle 的 m1ddc 稳定读出真值（15/7、写入生效），而我们**所有
同源复刻**（kvmprobe shim 四格矩阵、纯 C linkorder、ObjC-main）全部回罐头帧。
MVP 的 Mac DDC 路径当前唯一候选方案是「sidecar 调起自建 m1ddc 子进程」——但
**bottle 能工作 ≠ 自建能工作**。本实验用原源码原样构建一次，直接判定：

| 自建版真机结果 | 结论 |
|---|---|
| 读出 15/7（与 brew bottle 一致） | **产品路径成立**：vendoring 自建 m1ddc 子进程方案定案（MIT 许可，归属保留） |
| 回罐头帧 | bottle 与其自身源码存在未知构建期差异——记录为封箱根因备注，产品需另行出路（如分发前逐一验证的 bottle 级构建） |

## 构建

```bash
cd probes/mac/experiments/m1ddc-stock
./build.sh      # 产物 probes/mac/.build/m1ddc-stock/m1ddc-selfbuilt（附 SHA-256 / 签名 / 链接框架）
```

## 真机运行协议（会话间隙；m1ddc 是读写工具，本实验只跑 get，无写入无物理动作）

```bash
cd kvmflow
git fetch && git checkout kvm1-probes-readfix && git rev-parse HEAD   # 校验 commit
cd probes/mac/experiments/m1ddc-stock && ./build.sh
# 同一分钟背靠背（对照三方：brew bottle、自建版）：
m1ddc display 1 get input                              # brew bottle 参照
../../.build/m1ddc-stock/m1ddc-selfbuilt display 1 get input
../../.build/m1ddc-stock/m1ddc-selfbuilt display list  # 记录编号（外接屏编号以 list 为准）
# 对每台在线外接屏各跑一次 bottle get + 自建 get，逐条记录时刻/退出码/stdout
git checkout kvm1-probes                               # 回会话钉死分支
```

注意：自建二进制名 `m1ddc-selfbuilt` 与 PATH 里的 brew `m1ddc` 不同名，不会误调；
两工具输出形态相同（纯数字）。

## 证据等级

- 交付时：vendored 源码 SHA-256 固定 + 自带 Makefile 构建成功（`-Werror` 通过）
  + 开发机烟测（`display list` 正常、无外接屏时 get 正确报「Could not find a
  suitable external display」）= **simulated**。
- 家庭 Mac 实机运行 = **machine_observed**，判定归协调者。
