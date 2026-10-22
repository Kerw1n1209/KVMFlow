# T4 混屏前端与托盘交付记录

日期：2026-10-21。分支 `feat/mixed-ui`，已合并 `feat/mixed-core` 至 `ed88435`。
当前契约以 [MIXED_PROTOCOL.md](../../sidecar/MIXED_PROTOCOL.md) 为准。

## 已实现

- 状态页：模式条、全部屏幕（含内屏和保留的离线分配）、电脑卡片、键鼠持有者、下一次物理切换预告、提示条、最近一次切换与诊断入口。
- 设置：混屏开关、屏幕分配、内屏固定属于本机、全部 validation reasons、未分配提示、同步状态和保存/失败反馈。
- 电脑编辑器移除旧 follow 控件，保留历史 follow、layouts、arrival IDs 作为迁移档案。输入值继续在「电脑与显示器」编辑。
- 状态页的 USB 端口图删除。电脑卡片已直接展示端口号；重复绘制硬件端口图不能提供新的操作信息。诊断保留配置和切换日志。
- 托盘：当前键鼠电脑/端口、混屏分配摘要、勾选开关和设置入口；本机与远程模式变化均通过完整 state 去重通知。没有手动切屏菜单。
- 所有新增产品文案有中英文版本。使用本地显示器顺序和 runtime 默认编号，用户命名原样保留。

## 状态与证据

- 仅 `mixed.set` / `mixed.assign` 发起混屏操作。普通配置保存保留现有分配；核心负责 config.set 的 mixed 所有权保护。
- 响应成功只表示保存。界面读取 `transition.phase/outcome`、结构化 `execution[].error`、warnings 和 pending；恢复失败提示下次按切换器恢复。
- `current_device_id=null` 时显示目标电脑及命令/目标证据，不显示「未知」或声称画面已经确认。内屏消失显示「已合盖或不可用」。
- H 未知不假定在本机；`holder_confidence=inferred` 明确显示「预计」。
- 数字 revision 相同也比较 `section_meta.updated_at_ms/updated_by`；过期 pending 通知不能覆盖新分配/模式。
- 未分配与最后屏幕提示可以关闭，并按切换组及稳定 code/fingerprint 保存关闭选择。重复 warning 通知不反复记同一提示。
- 所有托盘菜单变更在 `run_on_main_thread` 中读取最新快照；调用原生 API 前释放锁，避免后台任务与菜单点击互等。

## T2 可选接入点

T2 未交付的内容不展示占位数据，不调用虚构的 LAN RPC：

- 可选 `state.lan.peers[]`：`device_id`、`name`、`online`、`synced`。
- 缺少 online 不显示连接状态；只有 online/synced 都显式为 true 才显示已同步。
- 配对入口在 peer 的 `pairing_available=true` 时显示；宿主提供
  `kvmflow.lan.openPairing(device_id)` 时优先转发给宿主，否则进入
  应用内配对流程（见 `docs/pairing-ui.md`）。

配对弹窗和初始化向导已在 `feat/pairing-ui` 交付，记录见
[pairing-ui.md](pairing-ui.md)。

## 与设计稿的差异

- 沿用现有项目字体与组件；屏幕 220×128、电脑卡片 240 宽、分配下拉 168 宽，均已用 DOM 测量。
- 增加命令证据、未知/推算键鼠位置、未完成恢复及模拟数据提示，避免将保存或 DDC 接受误称为真实画面确认。
- 没有 T2 数据时隐藏设计中的 LAN/同步状态。
- 原生 disabled 菜单项收不到点击，所以置灰的混屏勾选项旁增加「设置混屏模式…」。
- 三屏在正常与最小窗口同排缩放；小窗口使用自然纵向滚动。

## 验证

| 检查 | 结果 |
| --- | --- |
| `npm test`（desktop） | 93/93 通过 |
| `npm run smoke`（desktop） | 7/7 通过 |
| `cargo test --manifest-path sidecar/Cargo.toml` | 192 项通过 |
| `cargo test --manifest-path desktop/src-tauri/Cargo.toml` | 15/15 通过 |
| `npm run test:rust`（desktop） | 预处理失败，`iconutil: KVMFlow.iconset:Invalid Iconset`；不能计作通过 |
| 实际浏览器模拟预览 | 21 张截图；普通/混屏/内屏离线/恢复中/恢复失败/单屏/合盖/三屏/全给一台/未分配/英文/最小窗口；无横向溢出 |
| 浏览器交互 | 分配变更、混屏关闭、深链接定位已运行；控制台 0 错误 |

包装命令使用 `CLANG_MODULE_CACHE_PATH=/tmp/kvmflow-mixed-unique-clang` 越过默认缓存目录权限限制后，停在上述 iconutil 问题。直接 Cargo 测试使用已有本地 m1ddc 构建资源，仅用于链接，不执行显示器控制。

截图与完整日志只保存在本地设计目录，未加入仓库。

## 复现预览

```sh
node desktop/scripts/preview-mixed.js
# http://127.0.0.1:4176/?scenario=mixed&page=settings&lang=en
```

服务仅监听 127.0.0.1，注入模拟宿主给实际 renderer；不启动 sidecar、不读取真实配置、不发送 DDC 或断开显示器。fixtures 独立放在 tests，不进入产品 renderer。

可选 scenario：`normal`、`mixed`、`builtin`、`builtin-offline`、`single`、`closed-lid`、`three`、`all-to-one`、`unassigned`、`restoring`、`restore-pending`。

## 验收边界

未进行真实 USB/DDC、跨机 LAN、OS 显示器断开、系统通知展示或原生托盘竞争压力的真机验收。现阶段有渲染、交互、原生编译和纯规则测试证据；T2/T3 联调与硬件验收继续由后续任务完成。


## 独立审查修正

5 个 P2、1 个 P3、两处设计偏差及 XSS 用例问题均已处理：

1. 暂停结果通过统一入口合并局部字段，不清空混屏勾选、分配和开关可用性。
2. worker 发布状态时赋予 `runtime_seq`；前端与托盘按字段拒绝晚到的旧值。主线程通知和菜单任务执行时均读取最新快照，调用原生 API 前释放锁。
3. 安装期间禁用混屏菜单，点击与后台执行前重复检查 installing；已排队的操作也不会绕过门禁。
4. 已分配屏幕只能改选电脑；未分配仅为禁选占位。界面事件和分配计算都拒绝主动清空。
5. 请求错误与实际恢复结果分离；idle/ok、无 pending、无执行失败的权威关闭状态会清除旧错误。部分恢复时继续显示待恢复提示，诊断记录保留请求错误。
6. 告警按活跃周期去重，条件解决后复发会重新提示；关闭的对应横幅也按周期恢复。
7. E2 在开关关闭时就显示合盖限制；内屏 180×112、宽底座 200×6，外屏短支架 44×4。
8. XSS 用例移除遮蔽恶意 display_name 的配置 label，并正向断言恶意文本确实出现在两个页面，再断言未产生图片节点。

### 需要 T1 同步的最小后端扩展

`1a41057` 在本分支加入 `runtime_seq`，改动位于：

- `sidecar/crates/sidecar/src/lib.rs`：在同一 worker 内为状态通知与状态响应统一编号，不落盘、不跨机同步。
- `sidecar/crates/sidecar/tests/embedded_runtime.rs`：验证完整状态、暂停局部响应、混屏响应、通知的共同单调序号。
- `sidecar/MIXED_PROTOCOL.md`：记录字段和消费者规则，协议版本仍为 1。

最新 pending ID 的六元组作为不透明字符串处理，不在前端拆解。

### 回归证据

新用例在修复前已运行失败。另将 `fe564b3` 的 renderer 与测试 harness 解包到独立临时目录，放入当前新增前端用例，7 个用例全部失败；当前完整 npm test 93/93 通过。暂停、原生旧快照和安装保护的 Rust 用例也分别观察到修复前失败、修复后通过。

已检查状态通知、初始 state.get、mixed 命令响应、pending 通知及托盘异步请求这些相同模式的入口，拒绝被更新字段取代的旧值。浏览器实测 E2 中英文、最小窗口、未分配占位、关闭请求失败后从部分恢复到成功恢复；无横向溢出，控制台 0 错误。

`impl/REVIEW.md` 列出本次更新的 8 张截图；被替换的 4 张旧图保存在 `impl/review-before/`。红绿测试和最终运行日志位于 `impl/verification/review/`。全部图片是模拟预览。

### 统一复审追加修正

- `48dded2` 将 renderer 与 native 快照改为按顶层字段维护序号，mixed 整体作为一个字段。mixed.set(seq 5) 先到、完整暂停状态(seq 4) 后到时，补齐 enabled=false，保留 mixed(seq 5)。反向交错也不能用旧 enabled=true 覆盖已暂停状态。app.js 读取合并结果。
- 消费者支持 `runtime_generation`，缺省为 0；先比较 generation，再在同一代内比较字段 seq。新一代清空缓存与序号，旧一代连尚未出现过的字段也不能补入。支持 nested mixed stamp 与 pending 通知的 generation 传递。generation 的生产留给 display-detach 集成，协议见 `sidecar/MIXED_PROTOCOL.md`。
- 无序号的 runtime.error 事件独立记录，不作为旧快照丢弃；保留字段序号，晚到旧快照不能消除错误。错误事件也检查 generation，旧 worker 的错误不能覆盖新 worker 状态；display-detach 需同时给错误事件附上 generation。
- 保留 mixed_menu 的两处 installing 检查及安装时菜单置灰。安装锁到 mixed.set 入队的竞争窗口仍由 display-detach 的 released_topology worker 门禁处理。
- 本轮只运行相关测试：renderer mixed 与 client-shell 76/76；Rust runtime_state 7/7、mixed_menu 5/5。新增暂停交错与 generation 用例均先观察到失败再修复。未进行硬件断屏或真机验收，无页面布局变化。

### Iteration 1 集成

RuntimeSlot 已为 state.get、暂停与 mixed 命令响应、全部通知 payload 赋予来源
runtime_generation。native last_state 与 renderer 共同按 (generation, seq) 合并；
新 worker 清空旧代字段游标，旧 worker 的状态与错误无法覆盖新状态。
跨 native/jsdom 的模拟 worker 重建测试覆盖第一条 mixed.changed、完整 state 与迟到消息。
安装竞争窗口由 released_topology worker 门禁冻结，回归测试覆盖最后一次 installing
检查到 mixed.set 入队之间发生安装释放，并验证配置未变、无 DDC/detach、新恢复后可操作。
本次集成保留 main 的 Rust 测试准备修复；下方历史分支验证数据仍只描述各自交付时点。
真机托盘、通知与显示器验收边界保持不变。
