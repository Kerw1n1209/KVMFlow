# 配对前端与初始化向导交付记录

日期：2026-10-22。分支 `feat/pairing-ui`（基于 `feat/lan-runtime`）。
契约以 [LAN_RUNTIME_PROTOCOL.md](../../sidecar/LAN_RUNTIME_PROTOCOL.md)
为准。实现截图与设计稿对比见主仓库
`docs/design/iteration-1/pairing/impl/README.md`（不入库）。

## 已实现

- 配对弹窗：发现列表选择 → 连接 → 6 位数字比对（双方确认标志、
  不一致即取消）→ `await_resolve`（混屏分配冲突二选一、pending 输入
  与 blocker 摘要）→ 提交中 → 完成（含 `config_merged:false` 的
  「稍后合并」与 `peer_confirmed_commit:false` 的等待提示）。
- 主力机向导：第 3 步新增「添加另一台电脑」入口（局域网优先，保留
  手动填写路径）；副机就绪后显示其上报的显示器与输入值（pending
  行给出「用显示器按键切一次」的指引并禁用完成按钮）；
  第 4 步「试一下」汇总两次 `switch.report` 的逐屏结果与失败提示。
- 副机向导：角色选择（这是主机/副机）→ 调用 `lan.setup.secondary`
  并打开配对窗口 → 等待主力机（含 SAS 面板）→ 完成页列出本机仍未
  读到输入值的显示器。离开页面关闭配对窗口，恢复侧栏状态。
- `E_LAN_*` 错误码全部映射为中英文用户文案（未知代码回退为
  「配对失败（代码）」），覆盖：拒绝、取消、中止、超时、单端提交、
  提交失败、事务记录、中断、未确认、窗口关闭、断连、忙、地址、
  未启用、网络、组冲突、协议不兼容。
- 所有新增文案有中英文版本；新增 i18n 键与目录一致性由
  `tests/i18n.test.js` 保证。

## 状态机语义（对齐预留的后端变更）

1. **先确认的一方显示「本机已确认，等待对方确认」。** 状态机从
   `phase` 推导 `local_confirmed/peer_confirmed`（TODO 已标注：字段
   上线后直接读取）。先确认后不再出现「一致/不一致」按钮。
2. **提交中持续数秒。** 发起方收到 `ready_to_commit` 即视为提交
   开始（显式 `committing` 通知可能不单独出现），期间全部按钮禁用。
3. **配对窗口默认关闭。** 只有配对界面激活时调用
   `lan.pair.window {open:true}`；离开界面关闭；会话
   failed/cancelled/committed 由运行时关窗，前端不重复发送。

## 已落地的后端字段（2026-10-22 接入）

- `lan.peers[].platform`：发现列表直接显示 Windows/macOS；未知时只显示
  IP，不猜测系统。
- `lan.pair` 每条通知及配对 RPC 回复的 `session_id` / `event_seq`：
  UI 用它们丢弃迟到、重复和旧会话事件；`lan.pair.start` 回复 seq 为 0。
- 显式 `local_confirmed` / `peer_confirmed`：不再按 `phase` 推导。
- `lan.self.addr`（纯 IP，不带端口）：副机等待页显示「本机：名称 · IP」。
- `lan.profile` 的 `monitors[].input.value`：副机上报后，主力机确认页
  在收到 `lan.profile` 后渲染。

## 视觉修订（第二轮）

按设计稿截图修正：弹窗去掉 dialog 默认蓝色焦点框；pair-select 宽度、
选中态、搜索提示和 IP（不带端口）对齐；main-2-ready 的段间距、
chip、待读取实线卡片、底部行布局和小窗口滚动；副机等待页去掉步骤条、
补本机 IP、按钮与手动入口同行；角色选择去掉单选圆点；试一下进入
第 4 步；冲突页标题与卡片文案对齐。已读取显示器卡片按设计稿去掉
「输入值 N」行，输入值仍可在设置的「电脑与显示器」中查看。

## 验证

| 检查 | 结果 |
| --- | --- |
| `npm test`（desktop） | 129/129 通过（新增视觉/步骤相关 4 项） |
| `npm run smoke`（desktop） | 8/8 通过 |
| 浏览器模拟预览 | 27 张截图；中文/英文、单屏/三屏、pending、失败、提交中、等待对方确认；控制台 0 错误 |

首轮 `secondary-2-wait` 截图错误地停在「确认 USB Switch」：预览脚本在
`bootstrap.js` 异步加载完成前就调用了 `KVMFlowPairing`，脚本提前退出。
页面渲染本身正常；预览现会等待渲染 API 与初始状态就绪。

包装与硬件边界与混屏交付一致：未执行真实断开显示器、mirror 或
DDC 命令；未运行 `#[ignore]` 硬件测试；未做真机跨机联调。

## 复现预览

```sh
node desktop/scripts/preview-pairing.js
# http://127.0.0.1:4177/?scenario=pair-sas&page=wizard&lang=en
```

服务仅监听 127.0.0.1，注入模拟宿主给实际 renderer；不启动 sidecar。

## 2026-10-22 配对审查修复

保留 `dc9930e`、`18f33bc`、`0e942d9`、`d2762b4` 的视觉布局。

| 审查项 | 修复与回归覆盖 |
| --- | --- |
| P1-1 | 名称与错误详情按文本渲染，不能生成配对控件。 |
| P1-2 | 首次主机连接前保存后端真实 device_id、本机实测输入和 trigger；不创建远端输入、切换组或信任成员。 |
| P1-3 | 完成和 owner profile 后串行加载最新配置；设置保存保留新拓扑、未保存表单和排队中的编辑。 |
| P1-4 | 会话代次、目标设备、可选 session_id/event_seq、状态转移与重复操作保护；保留 pin-only 结果。 |
| P1-5 / P2-7 | Esc、dialog close、导航、pagehide、native hide 清理；失败关窗重试，副机显式重开。原生宿主串行关闭入站窗口、取消配对和停止测试。 |
| P1-6 / P2-8 | monitor_aliases、replacement 与 mixed 按协议提交；默认高亮与提交值一致。 |
| P2-9 | 提交弹窗继续到 profile 确认页；缺少 profile 或有 pending 时禁止试切。 |
| P2-10 / P2-11 | 完成和离开均停止试切；按方向去重；只有明确关联的双方证据齐备才显示双机指令已验证。 |
| P2-12 | 从原始 peers 查找设置入口目标；有信任 pin 的成员用 lan.connect.manual 重连。 |

新增回归位于 `tests/lan-pairing-regressions.test.js`、
`tests/bridge.test.js` 和 `main.rs` 的 `pairing_lifecycle_tests`。
新增失败路径均先运行失败测试再修复。

### 关联信息已落地（2026-10-22）

- `lan.pair` 通知与 RPC 回复带 `session_id`、`event_seq`，覆盖
  failed/cancelled/peer_committed；`lan.pair.start` 回复 seq 为 0。
- `lan.test.start` 回复 `test_id`；测试期间 `switch.report` 带
  `test_id`、`press_id`、`reporting_device_id`、`test_kind`；`lan.test`
  聚合为 `{test_id, events:[{device_id,kind,body}]}`。前端按
  `press_id + target_device_id` 关联双方证据，分别显示「指令已发送」与
  「画面已到达」，两者都成立才标记通过。
- `monitor_aliases[<joiner 指纹>] = false`（或 `null`）明确表示“不同
  显示器”：后端保留 `monitor_unmatched` blocker 并 pin-only 提交，UI
  显示「配置稍后合并」及 blocker 摘要，不再取消配对。
- `local_confirmed/peer_confirmed`、`lan.peers[].platform`、
  `lan.self.addr` 均为正式字段；平台缺失与本机地址缺失仍按协议省略显示。

未修改 `sidecar/`，未执行真实显示器断开、mirror 或 DDC 命令。截图存于
主仓库 `docs/design/iteration-1/pairing/impl/` 的 gitignore 目录，不提交。


## 2026-10-22 合并后契约对齐

`feat/lan-runtime` 合入 `feat/pairing-ui`（9dc7eb4）后，前端删除了全部
`TODO(backend)` 与按 phase 推导确认标志的回退，直接消费正式字段。新增
「同一台 / 不是同一台」显示器选择卡片（复用 `pair-option`），拒绝时提交
`false`；pin-only 完成页复用 `pair-resolve` 的 `pair-pending` 样式展示
`monitor_unmatched`。试切卡片在已有报告后并列显示两个 `settings-tag`：
「指令已发送」与「画面已到达」。视觉 token、布局和步骤条未改动。


## 2026-10-22 复核 P2/P3 修复

- **配对窗口收尾（N7）：** 后端在发起方本地提交、应答方发出 `pair.committed`、
  发起方收到 `pair.committed`、以及 `lan.pair.cancel` 时关闭入站配对窗口；
  入站拒绝在发送自己的 hello 之前判定，因此拒绝码作为第一帧
  `E_LAN_PAIR_WINDOW` 返回。前端在 `committed` / `peer_committed` 时也显式
  发送一次 `lan.pair.window{open:false}`，避免丢帧后窗口残留。
- **旧记录事件（N8）：** 事务记录的 `session_id` 不再覆盖当前配对会话；
  记录确认/撤销事件携带记录自己的 id。前端在有新会话时会丢弃旧 id 事件，
  空闲时会接收记录终态事件，二者都不会替换正在进行的配对界面。
- **拒绝证据（N9）：** 只有 `E_LAN_NOT_MEMBER` 的 Noise 认证密钥与记录中
  该设备的公钥一致（且地址匹配记录监听地址）时，才作为单边证据撤销；
  冒充者不能撤销 pin，fe80 地址不再互相等价。
- **记录确认（P3）：** 任意非拒绝帧确认记录前，核对其认证密钥与记录公钥。
- **对端名称（P3）：** 后端从已认证的 hello 取 `peer_name`（配置里的主机名），
  随 `lan.pair` 通知下发；`lan.profile` 也带 `name`。前端优先使用认证名称，
  不再使用 mDNS 发现到的名字；旧端未提供时保持发现名（文档化兜底）。
