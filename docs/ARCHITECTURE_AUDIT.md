# CrossDesk 阶段 0 架构审计

审计日期：2026-07-25  
审计基线：`392af44cbe06a7a591db86856ad6ed2aa83958d8`  
上游仓库：`https://github.com/feschber/lan-mouse.git`  
目标拓扑：Windows 11 Pro AMD64 发送输入，Mac mini M4 Apple Silicon 接收输入，Mac 位于 Windows 右侧

## 1. 审计范围与限制

本报告覆盖仓库结构、应用启动、配置加载、输入捕获与注入、网络传输、设备授权、切屏状态机、异常恢复和构建边界。

审计证据来自：

- `docs/PRD.md` V1.0 的阶段 0 要求；
- commit `392af44c` 的源码静态阅读；
- Windows 11 上的 Rust 格式、检查、Clippy、测试和 release 构建；
- release 可执行文件的 `--version` 启动验证。

当前限制：

- 本报告完成的是 PRD 第 22 章阶段 0 审计，不代表后续功能与验收条款已经实现；
- Windows 未安装 GTK4、libadwaita 和 `pkg-config`，GTK 前端未编译；
- 当前没有 macOS 构建机，CoreGraphics/ApplicationServices 后端未编译或运行；
- 未进行两机联调、权限授权、切屏体验或性能测量；
- 仓库现有自动化测试数量为 0，测试命令通过不代表行为已经得到测试覆盖。

## 2. 结论摘要

1. 项目不是待重写的原型。捕获、注入、DTLS、授权、配置热加载和断连释放已经形成可复用的完整主链路。
2. Windows 无 GTK 核心工作区可以用 Rust `1.97.1` 编译、Clippy、测试和构建 release；生成的 `lan-mouse.exe` 能正常输出版本信息。
3. 现有协议以 Linux evdev 扫描码为跨平台中间表示，Windows 与 macOS 后端负责各自转换，不应另建第二套扫描码协议。
4. 当前最高风险是 Windows 捕获侧鼠标与键盘共用容量 10 的有界队列，队列满时所有事件都可能被丢弃，包括 `KeyUp`。
5. 协议缺少 `ReleaseAll`、输入序号和状态快照，无法快速检测并修复发送端与接收端的按键状态漂移。
6. 切屏仅有 `WaitingForAck/Sending` 两个状态，没有累计阈值和冷却；语义键位映射也尚未实现。
7. 下一阶段应先建立可观测基线，再做输入可靠性改造。没有测量数据前不应进行大范围低延迟重构。

## 3. 仓库与 crate 边界

| crate/模块 | 责任 | 主要入口 |
|---|---|---|
| `lan-mouse` | 服务编排、配置、连接、状态机 | `src/main.rs`、`src/service.rs` |
| `input-event` | 跨平台输入事件与扫描码 | `input-event/src/lib.rs`、`scancode.rs` |
| `input-capture` | Windows/macOS/Linux 输入捕获 | `input-capture/src/lib.rs` |
| `input-emulation` | Windows/macOS/Linux 输入注入 | `input-emulation/src/lib.rs` |
| `lan-mouse-proto` | 输入与控制事件的手写二进制编解码 | `lan-mouse-proto/src/lib.rs` |
| `lan-mouse-ipc` | 前端与服务之间的 IPC | `lan-mouse-ipc/src/lib.rs` |
| `lan-mouse-cli` | 命令行前端 | `lan-mouse-cli/src/lib.rs` |
| `lan-mouse-gtk` | GTK/libadwaita 前端与 macOS 菜单栏集成 | `lan-mouse-gtk/src/lib.rs` |

依赖方向保持清晰：平台后端产出或消费 `input-event`，根服务负责编排，协议层只负责编解码。改造应继续沿用这些边界。

## 4. 应用启动流程

`src/main.rs` 的启动路径如下：

```text
main
  -> 初始化 env_logger
  -> run
      -> Config::new
      -> 根据 CLI 子命令分流
          -> test-emulation / test-capture / cli
          -> daemon -> run_service
          -> 无子命令
              -> gtk feature: 启动 daemon 子进程，再运行 GTK 前端
              -> 无 gtk feature: 当前进程直接运行 daemon
  -> run_service
      -> Service::new
      -> Service::run
```

`Service::new`（`src/service.rs:80`）按顺序完成：

1. 从配置恢复客户端；
2. 加载或生成 DTLS 证书并计算指纹；
3. 创建前端 IPC listener，借此检测重复服务实例；
4. 创建授权指纹集合、DTLS listener 和主动连接管理器；
5. 选择捕获与注入后端；
6. 创建 DNS resolver；
7. 组装服务状态。

`Service::run`（`src/service.rs:130`）恢复启动时激活的客户端，然后用单线程 Tokio `select!` 同时驱动 IPC、捕获、注入、DNS、配置热加载和 Ctrl+C。正常退出时依次终止捕获、注入与 DNS。

### 启动路径风险

- 多处 channel/lock 操作用 `expect`；release profile 为 `panic = "abort"`，异常关闭会直接终止进程；
- GUI 模式的 daemon 是子进程，平台退出路径不同，需要分别验证 Windows 与 macOS；
- 无 GTK 构建时 `start_service` 不会被调用，产生一个可接受但应记录的 `dead_code` 警告。

## 5. 配置加载与持久化

配置入口为 `Config::new`（`src/config.rs:342`）：

```text
CLI --config
  -> 否则使用平台默认目录/config.toml
  -> 创建配置目录
  -> 文件不存在时写入默认 TOML
  -> 解析 ConfigToml；解析失败记录 warning 并以 None 继续
  -> CLI --cert-path > TOML cert_path > 默认 lan-mouse.pem
  -> 创建 notify watcher 并监控配置目录
```

优先级为 CLI 参数高于 TOML，高于内建默认值。Windows 默认目录来自 `LOCALAPPDATA`，Unix/macOS 来自 `XDG_CONFIG_HOME` 或 `$HOME/.config`。

配置热加载由 `Config::changed`（`src/config.rs:415`）监听创建、数据修改和删除事件，成功解析且内容变化后由 `Service::handle_config_change` 重建客户端并刷新 release bind 与授权指纹。

配置回写由 `Config::write_back`（`src/config.rs:550`）执行。当前会整体覆盖 TOML，源码已有 TODO 指出注释无法保留。

已确认的配置缺口：

- `ConfigToml`（`src/config.rs:64`）没有 `config_version`；
- 没有版本迁移入口；
- watcher 接收和文件系统事件使用 `expect`；
- `set_clients` 在客户端为空时直接返回，可能无法通过该方法持久化“清空所有客户端”的意图，需在功能测试中确认上层行为。

## 6. 输入事件主链路

Windows 发送到 macOS 的主链路为：

```text
WH_MOUSE_LL / WH_KEYBOARD_LL
  -> Windows 原生码转换为 Linux evdev 扫描码
  -> input-capture 有界队列
  -> InputCapture::poll_next
  -> CaptureTask::handle_capture_event
  -> ProtoEvent 编码
  -> DTLS 发送
  -> LanMouseListener read_loop
  -> EmulationTask
  -> InputEmulation::consume 去重与状态跟踪
  -> macOS CGEvent 注入
```

关键边界：

- Windows hook：`input-capture/src/windows/event_thread.rs:307`、`:334`；
- Windows 捕获队列：`input-capture/src/windows.rs:44`；
- 捕获状态机：`src/capture.rs:317`、`:428`；
- 发送：`src/connect.rs:123`；
- 协议：`lan-mouse-proto/src/lib.rs`；
- 接收：`src/listen.rs:248`；
- 注入状态跟踪：`input-emulation/src/lib.rs:139`；
- macOS 注入：`input-emulation/src/macos.rs:290`。

Linux evdev 扫描码是现有跨平台中间表示。键位语义策略应作为映射层叠加在物理码链路上，而不是替换事件模型。

### 6.1 Windows 后端调用链

捕获路径：

```text
WindowsInputCapture::new
  -> 创建容量 10 的 channel
  -> EventThread::new 启动 Win32 消息线程
  -> SetWindowsHookEx 注册 WH_MOUSE_LL / WH_KEYBOARD_LL
  -> mouse_proc / kybrd_proc
  -> to_mouse_event / to_key_event
  -> Windows 原生码转换为 Linux evdev 码
  -> try_send_event
  -> WindowsInputCapture::poll_next
```

边缘进入由 `display_util::entered_barrier` 根据前后光标位置与显示器矩形判断。显示器变化后，事件线程重新枚举有效显示区域。

注入路径：

```text
WindowsEmulation::consume
  -> Pointer/Keyboard 事件分派
  -> rel_mouse / mouse_button / scroll / key_event
  -> Linux evdev 码转换为 Windows scan code
  -> SendInput
```

已识别边界：`KeyboardEvent::Modifiers` 当前被忽略；`send_input_safe` 在 `SendInput` 持续失败时无限重试，没有退避或错误上报。

### 6.2 macOS 后端调用链

捕获路径：

```text
MacOSInputCapture::new
  -> 检查辅助功能与输入监控权限
  -> event_tap_thread
  -> CGEventTapCreate / CFRunLoop
  -> event tap 回调映射 PointerEvent / KeyboardEvent
  -> InputCaptureState::crossed 判断越界
  -> Tokio mpsc channel
  -> MacOSInputCapture::poll_next
```

事件 tap 被系统超时禁用时会重新启用；被用户输入禁用时会显示光标并清理捕获状态。Quartz 显示器重配置回调负责刷新 bounds。

注入路径：

```text
MacOSEmulation::consume
  -> Linux evdev 码转换为 CGKeyCode
  -> 维护 modifiers 与 pressed_buttons
  -> 创建 CGEvent
  -> CGEvent::post
```

已识别边界：按钮事件在 `input-emulation/src/macos.rs:392` 对鼠标位置使用 `unwrap`；release 构建采用 `panic = "abort"`，该路径失败会直接终止进程。

## 7. 协议、连接与授权

协议使用手写大端序编码，最大事件为 21 字节：`u8` 类型、`u32` 时间和两个 `f64`。当前事件类型包括：

```text
PointerMotion, PointerButton, PointerAxis, PointerAxisValue120,
KeyboardKey, KeyboardModifiers, Ping, Pong, Enter, Leave, Ack, Hello
```

`Hello` 携带 8 字节短 commit，用于软提示构建差异；它不是协议版本协商。

当前协议没有：

- protocol version；
- session ID；
- 通用 sequence；
- 发送时间戳；
- `ReleaseAll`；
- `InputStateSnapshot`。

未知事件解码失败时，连接读循环记录 debug 并继续，因此追加新事件类型具备有限的前向兼容性。协议变更仍应新增显式能力/版本判断，不能只依赖旧端静默忽略。

连接使用 DTLS。接收端在 `src/listen.rs:78-110` 验证客户端证书指纹是否存在于授权集合；未授权连接会上报前端等待用户处理。发送端每 500 ms 发送一次 ping，共 4 次，一轮无响应即关闭连接。

## 8. 切屏状态机

捕获状态仅有（`src/capture.rs:428`）：

```rust
enum State {
    WaitingForAck,
    Sending,
}
```

Windows 边缘判定在 `input-capture/src/windows/display_util.rs:58`，由上一位置仍在显示区、当前位置越界触发；macOS 使用预计位置越出 bounds 触发。

当前没有：

- 边缘驻留或累计移动阈值；
- 切换冷却时间；
- `Local/EdgePending/Switching/Remote/Returning/Recovering` 等显式阶段；
- 不同分辨率与缩放比例下的垂直位置映射证据。

应先通过真实双屏几何与日志确认抖动是否发生，再扩展状态机。不要一次性替换现有进入、确认和返回链路。

## 9. 已有异常恢复能力

以下机制应保留并补测试，不应推倒重写：

| 机制 | 位置 | 作用 |
|---|---|---|
| 本地 release bind | `src/capture.rs:325` | 默认四个左修饰键触发返回 |
| 返回前逐个补 KeyUp | `src/capture.rs:389` | 尽量释放远端已按键 |
| 返回时清零 modifiers | `src/capture.rs:404` | 清理与按键集合分离的掩码状态 |
| 接收端无消息看门狗 | `src/emulation.rs:209` | 超过 1 秒移除会话并释放状态 |
| 注入层按键去重 | `input-emulation/src/lib.rs:215` | 防止重复按下/释放 |
| destroy 释放按键 | `input-emulation/src/lib.rs:166` | 断连时清理注入状态 |
| macOS event tap 超时恢复 | `input-capture/src/macos.rs:446` | 重新启用 tap |
| macOS tap 被用户输入禁用处理 | `input-capture/src/macos.rs:469` | 恢复光标并清状态 |
| macOS 显示器热插拔 | `input-capture/src/macos.rs:633` | 刷新屏幕 bounds |
| 连接心跳 | `src/connect.rs:233` | 检测无响应连接 |

## 10. 潜在性能瓶颈与稳定性风险

### 10.1 潜在性能瓶颈

1. Windows 高轮询率鼠标的 Motion 与键盘/按钮共享容量 10 的 FIFO，既可能造成可靠事件排队，也可能造成状态事件丢失；
2. Motion 事件没有 latest-event-wins、累计位移或过期淘汰，网络抖动时可能播放历史轨迹；
3. 协议没有 sequence 与采样时间点，当前无法量化排队、发送、接收和注入各阶段耗时；
4. Windows `SendInput` 失败循环可能导致单核满载；
5. 当前没有队列长度、事件速率、合并数或丢弃数指标，任何优化都缺少前后数据。

### 10.2 潜在稳定性风险

#### P0：输入安全与状态一致性

1. **捕获队列可能丢 KeyUp**：Windows 捕获在 `input-capture/src/windows.rs:44` 使用容量 10 的共享队列，移动和键盘事件都通过 `try_send_event`；高速移动可能挤占可靠输入事件。
2. **没有 ReleaseAll**：返回时依赖逐个 KeyUp；任一数据报丢失后只能等接收端看门狗。
3. **没有序号和状态快照**：无法识别重复、乱序或过期输入，也无法在重连、唤醒和切换后校准状态。
4. **紧急释放默认键不符合 PRD**：现有默认值是 `LeftCtrl+LeftShift+LeftMeta+LeftAlt`，PRD F-10 要求 `Ctrl+Alt+Shift+Esc`；本地判定机制可复用，但默认配置和完整 ReleaseAll 语义仍需实现。

#### P1：切换与跨平台体验

1. **无阈值和冷却**：边缘越界即触发，存在误切和抖动风险。
2. **无语义键位模式**：Windows `Ctrl+C` 到 macOS 仍是物理 Control+C，不会自动变为 Command+C。
3. **Windows 忽略 Modifiers 事件**：`input-emulation/src/windows.rs:71` 对反向控制缺少掩码兜底。

#### P2：错误处理与可维护性

1. `input-emulation/src/windows.rs:108` 的 `SendInput` 失败无限重试，可能占满 CPU；
2. `input-emulation/src/macos.rs:392` 对可恢复的鼠标位置查询使用 `unwrap`；
3. 服务、网络和配置通道中存在多处 `expect("channel closed")`；
4. `Service::update_incoming`（`src/service.rs:411`）找不到客户端时 `expect`；
5. 配置无版本与迁移机制；
6. 全仓库没有单元或集成测试。

## 11. 不建议修改的边界

后续阶段应优先复用以下实现：

- 平台捕获与注入后端的 trait 和选择机制；
- Linux evdev 作为中间扫描码表示；
- DTLS 证书生成、指纹授权和连接管理；
- 注入层 `pressed_keys` 去重与 destroy 释放；
- macOS event tap 自恢复和显示器热插拔处理；
- 配置热加载与 CLI 覆盖优先级；
- 单线程 Tokio 主事件循环，除非测量证明它是瓶颈。

协议扩展应追加事件、能力和兼容分支，不应直接改写旧事件编码。高频输入不得进入 GUI 主线程，也不得用无界队列规避背压设计。

## 12. 阶段 0 验证结果

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| 核心 workspace `cargo check` | 通过，排除 `lan-mouse-gtk` 且关闭默认 feature |
| 核心 workspace `cargo test` | 通过，但共 0 个测试 |
| 核心 workspace `cargo clippy --all-targets` | 通过，存在 2 条冗余借用警告及条件编译警告 |
| 核心 workspace release build | 通过 |
| `target/release/lan-mouse.exe --version` | 通过，版本 `0.11.0`、commit `392af44c` |
| 完整 GTK workspace | 未通过环境门禁：缺少 `pkg-config` 与 GTK4 系统库 |
| macOS 编译与运行 | 未执行：没有 macOS 构建环境 |
| 双机端到端与性能 | 未执行 |

完整命令与限制记录在 `docs/IMPLEMENTATION_STATUS.md`。

## 13. 推荐修改点与下一阶段入口

建议按以下顺序推进：

1. 按 PRD 阶段 1 增加可关闭的可观测指标，并形成 `PERFORMANCE_BASELINE.md`；
2. 用当前 Windows 核心 release 与 Mac 官方包完成基线联调，记录真实 RTT、CPU、内存和事件速率；
3. 按 PRD 阶段 2 复用现有本地 release bind 与断连释放，补齐默认快捷键、`ReleaseAll`、按钮状态和状态快照；
4. 按 PRD 阶段 3 设计 A/B/C 类有界队列、Motion 合并、序号与过期淘汰；
5. 按 PRD 阶段 4 增加切屏阈值、冷却、比例映射和失败回滚；
6. 按 PRD 阶段 5 在现有 evdev 标准化链路上叠加语义键位映射；
7. 阶段 6、7 再处理 UI、配置、打包与发布，避免 UI 工作阻塞输入安全和性能验证。

## 14. PRD 阶段 0 覆盖映射

| PRD 要求 | 报告位置 | 状态 |
|---|---|---|
| 当前目录结构、核心模块职责 | 第 3 节 | 已覆盖 |
| 事件完整调用链 | 第 6 节 | 已覆盖 |
| Windows 后端调用链 | 第 6.1 节 | 已覆盖；Windows 核心已编译 |
| macOS 后端调用链 | 第 6.2 节 | 已覆盖；仅静态审计 |
| 网络协议调用链、DTLS、授权 | 第 7 节 | 已覆盖 |
| 配置加载流程 | 第 5 节 | 已覆盖 |
| 应用启动流程 | 第 4 节 | 已覆盖 |
| 切屏状态流程 | 第 8 节 | 已覆盖 |
| 潜在性能瓶颈 | 第 10.1 节 | 已覆盖 |
| 潜在稳定性风险 | 第 9、10.2 节 | 已覆盖 |
| 推荐修改点 | 第 13 节 | 已覆盖 |
| 不建议修改点 | 第 11 节 | 已覆盖 |
| 运行现有测试 | 第 12 节 | 已执行，实际 0 个测试 |
| 确认 Windows 构建方式 | 第 12 节、`IMPLEMENTATION_STATUS.md` | 已确认核心构建；GTK 依赖未安装 |
| 确认 macOS 构建方式 | `.github/workflows/release.yml` | 已确认需在 Mac 安装 GTK4/libadwaita/ImageMagick/librsvg 后本机构建，未实际执行 |

阶段 0 已完成。根据 PRD 第 26 章，在用户确认技术决策前不得进入阶段 1 编码。
