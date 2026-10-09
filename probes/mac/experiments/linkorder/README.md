# 链接序隔离实验（KVM-1 探针读缺陷时间盒副线）

> **结果（2026-09-13 10:37Z 真机执行，machine_observed）**：判读矩阵第三行成立——
> `linkorder-cd` 与 `linkorder-io` 均回罐头帧（同分钟 m1ddc 参照 15/7），链接/加载
> 顺序作为唯一原因**证伪**。真根因随后由协调者源码对账定位：0.1.8 shim 的发现层
> 移植自 m1ddc **master**，而真机参照是 brew **1.2.0**（tag `2549fec`）的迭代序
> 算法，两者在双外接屏机器上选中不同 proxy。修复见 kvmprobe 0.1.10（本分支，
> shim 发现层改 1.2.0 逐字复刻）。本实验保留作排除证据与复现工具；进程形态梯度
> 的最后一格见 `../objcmain/`（ObjC-main 变体）。

判定「初始化/加载顺序」假设：kvmprobe 与 m1ddc 在同一台家庭 Mac、同一对
DCPAVServiceProxy、同一分钟内，前者回罐头帧（`6e 88 … 88 30/49` 族）、后者读出
15/7，而两者的事务代码已经逐字相同（0.1.8 起 kvmprobe 直接调用 m1ddcShim）。
otool 录得两者 LC_LOAD_DYLIB 顺序相反：

- `/opt/homebrew/bin/m1ddc`：`CoreDisplay → CoreGraphics → IOKit`
- `kvmprobe`（SwiftPM，`Package.swift` linkerSettings 顺序）：`IOKit → CoreGraphics → CoreDisplay`

## 设计

`main.c` 是纯 C 驱动：只调用与 `kvmprobe ddc-raw` 完全相同的 shim 源码
（`../../Sources/m1ddcShim/m1ddcShim.c`）做 service 发现 + 一次 m1ddc 事务。
无 Swift 运行时、无 Foundation、测量路径上无任何 CoreGraphics 调用。

`./build.sh` 用 clang 把**同一份源码**编译成两个可执行文件，唯一差异是三个
framework 的链接顺序（构建脚本用 otool 自检 LC_LOAD_DYLIB 顺序，不符即报错退出）：

| 二进制 | 链接序 | 复刻对象 |
|---|---|---|
| `linkorder-cd` | `CoreDisplay → CoreGraphics → IOKit` | m1ddc |
| `linkorder-io` | `IOKit → CoreGraphics → CoreDisplay` | kvmprobe |

输出与退出码语义对齐 `kvmprobe ddc-raw`：`0` = 交换完成、`1` = 交换失败、
`2` = 无外接 AV service。每行输出带 `link_order=`（含构建 commit），证据自描述。

## 构建（无需 SwiftPM/Xcode，只需 clang + otool）

```bash
cd probes/mac/experiments/linkorder
./build.sh
# 产物：probes/mac/.build/linkorder/linkorder-cd 与 linkorder-io
```

## 真机运行协议（会话间隙；显示器保持 Mac 输入）

全部为 Get 读事务，**无 VCP 写入、不改输入源、无物理动作**。

```bash
cd kvmflow
git fetch && git checkout kvm1-probes-readfix && git rev-parse HEAD   # 校验 commit
cd probes/mac/experiments/linkorder && ./build.sh
../../.build/linkorder/linkorder-cd --list        # 外接屏 CG display id（G73 此前为 3，G52plus 为 2，重启后以 --list 为准）

# 同一分钟内背靠背，逐条记录时刻、退出码、完整 stdout：
m1ddc display 1 get input                          # G73 参照（此前真值 15）
m1ddc display 3 get input                          # G52plus 参照（此前真值 7）
../../.build/linkorder/linkorder-cd <G73_id> --vcp 60
../../.build/linkorder/linkorder-cd <G52_id> --vcp 60
../../.build/linkorder/linkorder-io <G73_id> --vcp 60
../../.build/linkorder/linkorder-io <G52_id> --vcp 60

# 可选第三对照（与 linkorder-io 同链接序、Swift 进程）：
../../.build/release/kvmprobe ddc-raw <G73_id> --vcp 60
../../.build/release/kvmprobe ddc-raw <G52_id> --vcp 60

git checkout kvm1-probes    # 回到会话钉死分支（e05f227）
```

## 判读矩阵

| 观察结果 | 判定 | 后续 |
|---|---|---|
| `linkorder-cd` 解出 15/7，`linkorder-io` 回罐头帧 | **链接/加载顺序假设成立** | 修复 = 调整 `Package.swift` linkerSettings 顺序为 CD→CG→IOKit |
| 两个 C 变体都解出 15/7 | 纯 C 进程内链接序无关；若 `kvmprobe ddc-raw`（同 io 序、Swift 进程）仍罐头 → 差异锁定 Swift 运行时/Foundation 层 | 下一层隔离：Swift 最小复现 |
| 两个 C 变体都回罐头帧 | 链接序假设在纯 C 进程中**证伪**（作为唯一原因） | 剩余 m1ddc 差异候选：ObjC 运行时、Foundation、m1ddc main() 调用序列、其链接 `_IOAVServiceCreate` 而探针只用 `IOAVServiceCreateWithService` |
| `cd` 罐头、`io` 15/7（意外反转） | 反向因果 | 保留原始帧，逐层复查 |

## 证据等级

- 本仓库交付时：构建成功 + otool 链接序自检 + 开发机内置屏烟测（`no_av_service_for_display` 优雅降级）= **simulated**。
- 家庭 Mac 实机运行 = **machine_observed**；判定权与排期归协调者。
