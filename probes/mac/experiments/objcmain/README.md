# ObjC-main 进程形态实验（KVM-1 探针读缺陷副线·最终轮）

> **结果（2026-09-13 11:32Z 真机执行，machine_observed）**：判读表第二行成立——
> G52plus 同屏对照 m1ddc=7（真值）vs objcmain 罐头帧，**ObjC 进程形态排除**；G73
> 此时已被切离 Mac（m1ddc 读 0、objcmain 收到规范 Null Message `6e 80 be`，该格
> 不作同输入一致性判定）。协调者随后两项收尾排查亦排除：① 签名——m1ddc 为
> adhoc、linker-signed、无 entitlements，与我们的 clang/swift 产物同类；② 链接
> 库——m1ddc 多出的 Foundation/libobjc，objcmain 已带上仍失败。**至此链接序、前
> 置链、额外调用、ObjC 形态、签名/entitlements 全部排除；副线按协议封箱**，剩余
> 为 m1ddc bottle 独有的深层差异（工具链/构建期未知因素），会话分工维持
> 「m1ddc 读写、kvmprobe 记日志」。产品级后续见 `../m1ddc-stock/`（原源码自建
> 实验，不受封箱约束）。

> **定位**：0.1.11 四格矩阵（2026-09-13 11:24Z，8 格全罐头）已排除嫌疑 A（m1ddc
> 前置调用链）与 B（我们 create 与交换之间的额外调用）。剩余主嫌 C = 进程形态
> 层，且边界已收窄：kvmprobe 是带 Foundation/ObjC 运行时的 Swift 进程仍失败，
> 所以问题不是「有没有 Foundation」，而是 m1ddc 那种 **ObjC main 进程形态**（或
> 签名/加载形态）里的某种结构差异。本实验补齐梯度最后一格。**这是副线最终轮：
> 无论结果如何，判定回填本 README 后副线封箱，主线读数走 m1ddc 分工。**

## 设计

`main.m` 是 ObjC 入口（`@autoreleasepool`、Foundation 字符串流经全程），**全部
IOAVService/IOKit/CoreDisplay/CoreGraphics 调用仍由同一 shim 源码发出**
（`../../Sources/m1ddcShim`，与 kvmprobe ddc-raw、linkorder 二进制同源可比）。
调用序列 = m1ddc 1.2.0 的 `display <N> get input`：

1. `kvm_m1ddc_prelude(1)` —— `getOnlineDisplayInfos` 形态（含 EDID UUID /
   DisplayAttributes 属性读取，m1ddc 总是读）；
2. `kvm_m1ddc_transport_for_display_opts(lean=1)` —— 1.2.0 走查，create 与交换
   之间零调用；
3. `kvm_m1ddc_get_vcp` —— 共享 shim 交换；
4. `kvm_m1ddc_fill_proxy_evidence` —— 交换后补录 proxy path/ID。

构建自检：二进制必须含 `objc_msgSend` 引用（真 ObjC 代码生成），否则构建失败。

## 三级进程形态梯度

| 进程形态 | 二进制 | 真机结果 |
|---|---|---|
| 纯 C | `experiments/linkorder/linkorder-cd` / `-io` | **罐头帧**（2026-09-13 10:37Z） |
| Swift（含 Foundation/ObjC 运行时） | `kvmprobe ddc-raw` 0.1.11 四格矩阵 | **罐头帧**（2026-09-13 11:24Z，8 格全） |
| ObjC main | `experiments/objcmain/objcmain` | **罐头帧**（2026-09-13 11:32Z；G52plus 同屏 m1ddc=7 对照） |

## 构建

```bash
cd probes/mac/experiments/objcmain
./build.sh      # 产物 probes/mac/.build/objcmain/objcmain
```

## 真机运行协议（会话间隙；Get 读事务，无写入无物理动作）

```bash
cd kvmflow
git fetch && git checkout kvm1-probes-readfix && git rev-parse HEAD   # 校验 commit
cd probes/mac/experiments/objcmain && ./build.sh
../../.build/objcmain/objcmain --list               # 外接屏 CG id（此前 G73=3 / G52plus=2，以 --list 为准）

# 同一分钟背靠背：
m1ddc display 1 get input                          # G73 参照
m1ddc display 3 get input                          # G52plus 参照
../../.build/objcmain/objcmain 3 --vcp 60          # G73
../../.build/objcmain/objcmain 2 --vcp 60          # G52plus

git checkout kvm1-probes                           # 回会话钉死分支
```

## 判读

| 观察结果 | 判定 | 后续 |
|---|---|---|
| objcmain 解出 15/7 | **C = 「ObjC 进程形态」坐实** | 产品修复方向：DDC 路径宿主于 ObjC 入口（或逐项对齐找到具体结构差异） |
| objcmain 仍罐头帧 | ObjC 形态解释也排除 | 剩余为 m1ddc 与三者皆不同的更深层差异（签名/entitlements、brew 工具链、运行时版本…）；**副线按协调者指示封箱**，读数维持 m1ddc 分工 |

## 证据等级

- 交付时：构建成功 + objc_msgSend 自检 + 开发机内置屏烟测（`discovery=none`、
  exit 2 优雅降级）= **simulated**。
- 家庭 Mac 实机运行 = **machine_observed**；判定回填后本 README 记录梯度结论并封箱。
