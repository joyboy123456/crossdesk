# CrossDesk 交接文档

交接时间:2026-07-25
交接内容:阶段 0(仓库审计)的代码勘察结果
接手方:Codex

---

## 1. 交接状态摘要

**已完成**:基线仓库克隆、PRD 第 22 章阶段 0 要求的全部代码定位与调用链梳理。

**未完成**:
- Rust 工具链未安装,`cargo fmt/check/clippy/test/build` **一条都没有运行过**
- `docs/ARCHITECTURE_AUDIT.md` 未产出(本文档是其前置材料,不能替代)
- 未做任何代码修改,工作区干净

**重要**:本文档所有结论均来自静态阅读源码,**没有任何一条经过编译或运行验证**。行号基于 commit `392af44`。

---

## 2. 环境现状

| 项目 | 状态 |
|---|---|
| 仓库路径 | `F:\1\CrossDesk` |
| 上游 | `https://github.com/feschber/lan-mouse` |
| HEAD | `392af44` (fix(ci): bust stale Homebrew glib cache on macOS runners) |
| 工作区 | 干净,无本地修改 |
| git remote | **未配置**(用 `git fetch` 拉的,origin 可能不完整,接手后请先 `git remote -v` 确认) |
| Rust 工具链 | **未安装**(无 cargo/rustc/rustup) |
| MSVC | 已有,2022 Community,`14.44.35207` |
| GTK4 / libadwaita | **未安装** |
| 现有测试数量 | **0**(全仓库无 `#[test]`,无 `tests/` 目录) |

### 目标设备拓扑(用户确认)

```
发送端:Windows 11 Pro (AMD64),键鼠物理连接端
接收端:Mac mini M4 (Apple Silicon / aarch64)
Mac 位于 Windows 右侧 → position = "right"
需求:一套键鼠控两台机器,光标右移进 Mac、左移回 Windows
```

用户已明确知晓这不是桌面延伸,Mac 屏幕显示的仍是 Mac 自己的画面。

### 构建环境注意事项

默认 feature 含 `gtk`,本机无 GTK4 时完整构建会失败。建议先用 `--no-default-features` 验证核心 crate。Windows 上 GTK 依赖需 gvsbuild 编译,耗时通常一小时以上(见 `.github/workflows/rust.yml`)。macOS 端需 `brew install gtk4 libadwaita imagemagick`。

macOS 版本**无法交叉编译**:需链接 CoreGraphics / ApplicationServices,必须在 Mac 本机构建。

---

## 3. 代码地图

### 3.1 Crate 结构

```
lan-mouse (root)          服务主体、状态机、配置、DTLS 连接
├── input-event/          跨平台事件定义 + 扫描码转换表
├── input-capture/        输入捕获(windows / macos / layer_shell / libei / x11 / dummy)
├── input-emulation/      输入注入(windows / macos / wlroots / libei / x11 / xdp / dummy)
├── lan-mouse-proto/      线协议编解码
├── lan-mouse-ipc/        GUI ↔ 服务 IPC(JSON over socket)
├── lan-mouse-cli/        CLI
└── lan-mouse-gtk/        GTK 前端 + macOS 菜单栏/权限引导
```

### 3.2 关键文件对照 PRD 阶段 0 清单

| PRD 要求 | 文件 |
|---|---|
| Windows 输入捕获 | `input-capture/src/windows/event_thread.rs`,入口 `input-capture/src/windows.rs` |
| Windows 输入注入 | `input-emulation/src/windows.rs` |
| macOS 输入捕获 | `input-capture/src/macos.rs` |
| macOS 输入注入 | `input-emulation/src/macos.rs` |
| 输入事件定义 | `input-event/src/lib.rs`,扫描码 `input-event/src/scancode.rs` |
| 网络发送 | `src/connect.rs`(发送端 / DTLS client) |
| 网络接收 | `src/listen.rs`(接收端 / DTLS server) |
| DTLS + 设备授权 | `src/listen.rs:78-110`(指纹校验回调),`src/crypto.rs` |
| 屏幕边缘切换 | `src/capture.rs`,Windows 几何判定 `input-capture/src/windows/display_util.rs` |
| 配置加载持久化 | `src/config.rs` |
| 服务主循环 | `src/service.rs:143-153` |
| 进程入口 | `src/main.rs` |

### 3.3 事件流完整调用链

**Windows 发送 → Mac 接收**:

```
Windows 低级钩子 WH_MOUSE_LL / WH_KEYBOARD_LL
  input-capture/src/windows/event_thread.rs:307 mouse_proc
                                          :334 kybrd_proc
  → to_mouse_event / to_key_event(平台码 → Linux evdev 扫描码)
  → try_send_event  [有界 channel(10),满则丢弃]
  → InputCapture::poll_next (input-capture/src/lib.rs:220)
      更新 pressed_keys,按 position_map 分发到 handle
  → CaptureTask::handle_capture_event (src/capture.rs:317)
      检查 release_bind → 状态机 WaitingForAck/Sending → 包装 ProtoEvent
  → LanMouseConnection::send (src/connect.rs:123)
  → ProtoEvent → [u8; MAX_EVENT_SIZE] (lan-mouse-proto/src/lib.rs:203)
  → DTLS conn.send
  ============ 网络 ============
  → read_loop (src/listen.rs:248) → ListenEvent::Msg
  → ListenTask::run (src/emulation.rs:140) 分派
  → EmulationProxy::consume → EmulationTask::do_emulation_session
  → InputEmulation::consume (input-emulation/src/lib.rs:139)
      update_pressed_keys 去重(防重复按下/释放)
  → MacOSEmulation::consume (input-emulation/src/macos.rs:290)
      evdev → mac CGKeyCode (keycode crate) → CGEvent post
```

**注意**:事件在 `input-event` 层统一用 **Linux evdev 扫描码**作中间表示,两端各自转换。这是既有设计,PRD 第 15 章的"物理键标准化"已经有基础,不要另起一套。

### 3.4 现有协议(重要:与 PRD 第 10 章差异很大)

`lan-mouse-proto/src/lib.rs:102-115`,`EventType` 全集:

```
PointerMotion, PointerButton, PointerAxis, PointerAxisValue120,
KeyboardKey, KeyboardModifiers, Ping, Pong, Enter, Leave, Ack, Hello
```

**协议中不存在**:序列号、时间戳、session_id、协议版本字段、ReleaseAll、状态快照(InputStateSnapshot)。

单包最大 `MAX_EVENT_SIZE = 1 + 4 + 8 + 8 = 21` 字节(u8 类型 + u32 time + 2×f64)。大端序,手写编解码,无 serde。

`Enter`/`Leave`/`Ack` 虽然带 serial 参数,但**实际全部硬编码传 0**(`src/capture.rs:415`、`src/emulation.rs:155,161,201`),serial 目前是死字段。

前向兼容机制已有:收到无法解码的报文只 `log::debug` 跳过,不断连接(`src/listen.rs:261-273`、`src/connect.rs:291`)。新增 EventType 时旧端会静默忽略。

### 3.5 现有切屏状态机(与 PRD 第 13 章差异很大)

`src/capture.rs:427-432`,只有两个状态:

```rust
enum State { WaitingForAck, Sending }
```

PRD 要求的六状态(Local / EdgePending / Switching / Remote / Returning / Recovering)**不存在**。

边缘触发是**越界即触发**,没有累计阈值、没有冷却时间:
- Windows:`display_util.rs:58 entered_barrier` — 上一帧在显示区内、当前帧越界即返回 Position
- macOS:`macos.rs:86 crossed()` — `location + delta` 超出 bounds 即触发

### 3.6 现有异常恢复能力(基线已做得不错,别推倒)

| 机制 | 位置 | 说明 |
|---|---|---|
| release_bind 释放 | `src/capture.rs:325` | 默认 `Ctrl+Shift+Meta+Alt`(四个左修饰键),本地判定 |
| 释放前补发 KeyUp | `src/capture.rs:389-398` | 遍历 `take_pressed_keys()` 逐个补 state=0,防对端修饰键卡住 |
| 释放时清零修饰掩码 | `src/capture.rs:404-411` | 单独发 Modifiers 全 0,因为对端 XKB 掩码与 pressed_keys 是两套状态 |
| 接收端看门狗 | `src/emulation.rs:209-220` | 每 5s 检查,>1s 无消息则 remove(addr) 释放按键 |
| 注入层按键去重 | `input-emulation/src/lib.rs:215` | pressed_keys 集合,防重复按下/释放 |
| destroy 时释放 | `input-emulation/src/lib.rs:166` | `release_keys` + Modifiers 清零 |
| macOS tap 超时自愈 | `input-capture/src/macos.rs:446` | `TapDisabledByTimeout` 原地 `CGEventTapEnable` 重启,保留捕获状态 |
| macOS tap 被杀 | `input-capture/src/macos.rs:469` | `TapDisabledByUserInput` 同步显示光标 + 清状态 |
| macOS 显示器热插拔 | `input-capture/src/macos.rs:633` | Quartz 回调刷新 bounds |
| 心跳 | `src/connect.rs:233` | 发 4 个 ping(间隔 500ms),一轮无任何回应则关连接 |

这些是上游长期踩坑修出来的,注释里写清了原因。改造时**先读注释再动手**。

---

## 4. 已识别问题(静态分析,未经运行验证)

按 PRD 第 28 章优先级(不失控 > 不丢按键状态 > 稳定连接 > 低延迟)排序。

### P0-1 捕获侧鼠标与按键共用有界队列,溢出丢 KeyUp

`input-capture/src/windows.rs:44` — `channel(10)`
`event_thread.rs:326,345` — 鼠标移动和键盘事件都走 `try_send_event`,失败仅 `log::warn` 后丢弃

高速移动鼠标时队列被移动事件填满,此时按键的 KeyUp 会被一并丢弃 → 对端修饰键卡住。虽有接收端看门狗(>1s)兜底,但 1 秒内的错误输入已经发生。

这同时命中 PRD 第 11 章(移动/按键必须分级)和第 28 章第二优先级。建议:按 A/B/C 类分流,A 类(Key/Button)走可靠路径不丢弃,C 类(Motion)单独合并。

### P0-2 无 ReleaseAll 事件

协议里没有这个消息类型。目前"释放"是靠发送端逐个补 KeyUp(`src/capture.rs:389`)实现的,一旦这些补发包在网络上丢失(UDP/DTLS 不保证送达),对端就得等看门狗超时。

PRD F-10/F-11 明确要求 ReleaseAll。需新增 EventType(利用现有前向兼容机制,旧端会忽略)。

### P0-3 无状态快照与序号,无法检测/修复状态不一致

PRD 第 12 章要求的 `InputStateSnapshot` 完全不存在;第 11.3 节要求的重复/乱序/过期检测也没有基础(无 sequence 字段)。

连接恢复、切屏完成、睡眠唤醒后,两端的按键状态无从校验。

### P1-1 切屏无阈值无冷却,可能抖动

见 3.5。用户两块屏若高度不同,或边缘有 Dock/任务栏,可能误触发或反复横跳。PRD F-04 要求 2px 边缘 + 6px 累计阈值 + 80ms 冷却,全部需新建。

### P1-2 键位映射不做语义转换(对用户体验影响最大)

`input-emulation/src/macos.rs:487` — 直接 `KeyMapping::Evdev → k.mac`,纯物理键位映射。

Windows 按 Ctrl+C,到 Mac 上就是 **Control+C 而不是 Command+C**。对开发者日常使用几乎是持续性硌手。PRD F-09 / 第 15 章要求的 semantic 模式需要新建。

考虑到用户是"Windows 主 + Mac 开发"的场景,这一项对主观体验的影响可能比 P0 的延迟优化更明显。建议在 P0 安全项完成后立刻做。

### P1-3 Windows 注入端完全忽略 Modifiers 事件

`input-emulation/src/windows.rs:71` — `KeyboardEvent::Modifiers { .. } => {}`

反向(Mac → Windows)时修饰键状态同步依赖逐个 Key 事件,没有掩码兜底。当前用户场景是 Windows 发、Mac 收,暂不触发;但 PRD 要求架构支持角色互换。

### P2-1 `SendInput` 失败无限重试,可能死循环

`input-emulation/src/windows.rs:106-114`:

```rust
loop {
    if SendInput(&[input], ...) > 0 { break; }
}
```

若因 UIPI / 权限 / 会话锁导致持续失败,这里会占满一个核心且不上报错误。违反 PRD 第 23 章第 9 条(不吞掉注入错误)。

### P2-2 macOS 注入端 unwrap 可能 panic

`input-emulation/src/macos.rs:392` — `self.get_mouse_location().unwrap()`

同文件其他位置都做了 `None` 判断,只有按钮事件这里 unwrap。release profile 是 `panic = "abort"`(`Cargo.toml:24`),panic 会直接杀进程。若此时对端正按住按键,则按键状态无人清理。违反 PRD 第 23 章第 8 条。

### P2-3 大量 `expect("channel closed")`

`src/service.rs`、`src/capture.rs`、`src/emulation.rs` 中通道操作普遍用 `expect`。配合 `panic = "abort"`,任一通道异常关闭即整进程终止。终止路径上没有"先释放远端按键"的保证。

### P2-4 配置无版本号、无迁移机制

`src/config.rs` 的 `ConfigToml` 无 `config_version` 字段。PRD 第 19 章要求带版本号 + 迁移逻辑。

已有的健壮性:解析失败会 `log::warn` 后带 `None` 继续启动(`config.rs:368-375`),不会因坏配置无法启动 — 这一点已符合 PRD 要求。

### P2-5 `update_incoming` 中的 expect 可能 panic

`src/service.rs:413-417` — `.expect("no such client")`。若 `incoming_conns` 与 `incoming_conn_info` 因竞态不一致则 panic。

---

## 5. 建议的下一步

### 立即要做(阶段 0 收尾)

1. **装 Rust 工具链**。用户对此有顾虑,已明确询问过是否影响环境变量,**动手前请再确认一次**。
   - Windows:rustup 只追加用户级 PATH `%USERPROFILE%\.cargo\bin`,不需管理员权限,`rustup self uninstall` 可干净卸载
   - Mac:改 `~/.zshrc` 加一行 source `$HOME/.cargo/env`;若用户介意可 `--no-modify-path` 装,代价是每次手动 source
   - 用户曾拒绝一次网络请求(抓 GitHub release),**意图未澄清**:可能是不愿联网、可能是不想用预编译包想直接编译。接手后请先问清

2. **跑通构建与检查**,补齐本文档缺失的验证结论:
   ```
   cargo fmt --all -- --check
   cargo check --workspace --no-default-features
   cargo clippy --workspace --all-targets
   cargo test --workspace          # 预期 0 个测试
   cargo build --release
   ```

3. **产出 `docs/ARCHITECTURE_AUDIT.md`**。本文档可作为素材,但 PRD 第 22 章要求的章节(配置加载流程、应用启动流程、不建议修改点等)需补全。

### 强烈建议:先验证基线再改代码

基线**当前就能满足用户的核心需求**(一套键鼠控两台、左右切屏)。上游有 Windows 和 macOS 预编译包。

建议先让用户跑通官方包,拿到真实基线体验,再决定改造优先级。理由:
- PRD 里的延迟目标(P95 ≤ 15ms 等)目前没有任何实测数据支撑,不知道基线离目标有多远
- 上面列的问题严重程度需要实测才能排序(比如 P1-1 切屏抖动是否真的发生,取决于用户两块屏的实际几何)
- 避免在没有基线数据的情况下做优化 — 违反 PRD 第 28 章"真实可测量数据优先于主观判断"

Mac 端部署要点:`xattr -rd com.apple.quarantine "Lan Mouse.app"`,然后授权辅助功能 + 输入监控两项权限。

### 改造顺序建议

```
阶段 1  可观测性(RTT / 各阶段耗时 / 队列长度)→ 建立 PERFORMANCE_BASELINE.md
阶段 2  P0-1 事件分级  → P0-2 ReleaseAll  → P0-3 序号与状态快照
阶段 3  P1-2 语义键位映射(用户体感收益最大)
阶段 4  P1-1 切屏阈值与冷却
随时    P2-1 / P2-2 这两个是小改动大收益,可以插空做
```

---

## 6. 必须遵守的约束(来自 PRD 第 9 / 23 章)

- **不得重写项目**。优先复用现有捕获/注入后端、加密与授权逻辑、事件类型与序列化方式
- 每次修改后 Windows 和 macOS **都要能编译**
- 不得虚构测试结果。平台限制导致无法运行的命令,须记录:未运行的命令 / 原因 / 预期平台 / 人工验证步骤
- 协议变更必须考虑版本兼容(现有前向兼容机制见 3.4)
- 不使用无界队列;不用 `unwrap()` 处理可恢复错误;不吞注入错误
- 高频输入处理不得进 GUI 主线程
- 性能优化必须有前后数据对比
- 每完成一阶段更新 `docs/IMPLEMENTATION_STATUS.md`

---

## 7. 待用户确认的决策

1. 是否安装 Rust 工具链(涉及两台机器的用户级环境变量)
2. 先用官方预编译包验证基线,还是直接从源码编译
3. 上次拒绝网络请求的真实意图
4. 两台显示器的分辨率与缩放比例(影响 PRD 第 14 章的跨屏垂直位置映射)
5. 是否需要保留上游 GTK 前端,或按 PRD F-12 改造为托盘/菜单栏优先
