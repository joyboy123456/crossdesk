# CrossDesk 实施状态

更新时间：2026-07-26  
当前基线：`392af44cbe06a7a591db86856ad6ed2aa83958d8`

## 阶段状态

| 阶段 | 状态 | 说明 |
|---|---|---|
| 阶段 0：仓库审计 | 已完成 | PRD 已归档，审计要求已逐项映射，并补充 Windows 核心构建证据 |
| 阶段 1：可观测性与性能基线 | 进行中 | Windows/macOS 核心指标构建已通过；真实双机输入样本待执行 |
| 阶段 2：输入安全与恢复 | 未开始 | ReleaseAll、状态快照与完整故障恢复尚未实现 |
| 阶段 3：低延迟事件管线 | 未开始 | 分级队列、Motion 合并、序号与过期检查尚未实现 |
| 阶段 4：切屏体验 | 未开始 | 阈值、冷却、比例映射与失败回滚尚未实现 |
| 阶段 5：Windows/macOS 键位映射 | 未开始 | 尚未修改映射层 |
| 阶段 6：UI 和配置 | 已实现，待双机验收 | egui 控制中心、四方向布局、授权、设置、托盘、文本剪贴板同步与 IPC 生命周期已实现 |
| 阶段 7：打包和发布 | 进行中 | Windows GUI 产物与 macOS bundle 元数据已更新；macOS 实机构建和签名待执行 |

## 本次完成内容

### 2026-07-26 屏幕节点拖拽调向

- 为已启用的远端屏幕节点增加 Lucide `grip-vertical` 拖拽手柄、抓取光标、悬停提示和可访问名称；
- 将拖拽判定改为卡片中心命中四方向槽位，支持命中容差、占用高亮、无效释放复位和原槽位忽略；
- 修正 egui 拖拽使用单帧 `drag_delta()` 导致释放时位移归零的问题，改为累计位移并缓存释放前卡片中心；
- 修复目标槽位高亮淡出期间释放鼠标会对已消失的拖拽目标执行 `expect`、导致 Windows release GUI 直接终止的问题，并将拖拽测试延长到高亮生效后的释放帧；
- 服务断开或方向待确认时禁用拖拽，离线但已启用的设备仍可调整配置方向；
- 四方向 IPC、配置格式、服务确认、三秒超时与断线回滚逻辑保持不变；
- `crossdesk-ui` 共 24 个测试通过，新增真实指针拖放、槽位命中、占用拒绝、禁用态、亮暗主题与响应式布局覆盖；
- 全工作区格式、检查、33 个测试、严格 Clippy 和独立目录 release 构建通过，产物为 `target/screen-drag-release/release/crossdesk.exe`。

### 2026-07-26 桌面前端重建

- 新增 workspace crate `crossdesk-ui`，使用 `eframe/egui 0.35`、`iconflow` Lucide 图标和系统中英文字体；
- 旧 `lan-mouse-gtk` 源码保留，但移出 workspace，默认构建不再依赖 GTK4/libadwaita；
- 新增 `crossdesk` GUI binary，Windows release 使用 GUI subsystem；`lan-mouse` CLI/daemon 子命令保持兼容；
- GUI 启动同一可执行文件的 daemon 子进程，仅在拥有子进程时发送 `ShutdownService`，超时后才强制结束；
- IPC 使用独立 Tokio 线程和有界通道，支持断线指数退避、连接后 `Sync` 和溢出重同步；
- 完成设备、授权、设置三个中文页面，以及设备新增、编辑、删除、DNS 解析、启停和状态提示；
- 完成屏幕 1/2+ 四方向布局、拖拽吸附、方向占用限制、服务确认前预览和断线/超时回滚；
- 完成长页面纵向滚动、待确认方向并发占用保护、断线操作拒绝和旧 IPC 请求清理；
- 新增 `CreateConfigured` 和 `ShutdownService` IPC 请求，不修改 UDP/DTLS 协议或配置 TOML；
- Windows/macOS 托盘或菜单栏支持重新打开与安全退出；
- 新增 Windows/macOS 双向 UTF-8 文本剪贴板同步、回环抑制、16 KiB 上限及设置页持久化开关；
- Hello 握手新增剪贴板能力位，只有新版本对端声明支持后才发送可变长度文本包；
- 移植 macOS 辅助功能、输入监控、授权后重启和撤权安全退出逻辑；
- 新增 IPC JSON、布局、表单、状态 reducer 和 `egui_kittest` 中文交互测试；
- 新增 `docs/DESKTOP_BUILD.md`，记录 `CrossDesk.exe`、`CrossDesk.app`、无 GUI 构建和平台验收流程。

- 核对仓库 HEAD、远程和工作区状态；
- 将用户提供的 PRD V1.0 原文归档为 `docs/PRD.md`；
- 安装并验证 Windows Rust stable MSVC 工具链；
- 复核启动、配置、捕获、注入、协议、DTLS、切屏与恢复链路；
- 按 PRD 第 22 章逐项补齐阶段 0 审计映射；
- 运行 Windows 无 GTK 核心工作区的格式、检查、测试、Clippy 和 release 构建；
- 验证 release 可执行文件能够启动并输出版本；
- 生成 `docs/ARCHITECTURE_AUDIT.md`；
- 新增默认关闭的 `metrics` feature，不改变协议线格式和队列策略；
- 实现 RTT、序列化、捕获分发到发送、接收到注入、切屏确认、事件速率和队列深度指标；
- 按类别统计 Windows 捕获队列满丢弃和接收端注入未启用丢弃；
- 增加结构化捕获状态转换日志，不记录实际键值或输入字符；
- 增加有界采样窗口与分位数单元测试；
- 生成 `docs/PERFORMANCE_BASELINE.md`，真实双机数据明确保留为待测；
- 通过 Tailscale SSH 在 Apple M4 Mac mini 上安装同版本 Rust，并完成源码同步与校验；
- 修正 `record_full_drop` 在非 Windows 平台产生的 dead-code 警告；
- 完成 macOS 无 GTK 指标开关、测试、Clippy、release 与 dummy 冒烟验证。

## 工具链

```text
host: x86_64-pc-windows-msvc
rustc: 1.97.1 (8bab26f4f 2026-07-14)
cargo: 1.97.1 (c980f4866 2026-06-30)
rustfmt: 1.9.0-stable
clippy: 0.1.97
```

`rustup` 已将 `%USERPROFILE%\.cargo\bin` 加入用户级 PATH。当前 Codex 进程通过完整路径调用工具；新终端会自动读取更新后的 PATH。

macOS 工具链：

```text
host: aarch64-apple-darwin
macOS: 26.3.1 (Build 25D2128)
rustc: 1.97.1 (8bab26f4f 2026-07-14)
cargo: 1.97.1 (c980f4866 2026-06-30)
rustfmt: 1.9.0-stable
clippy: 0.1.97
```

Mac 使用官方 rustup 用户级安装，未修改 zsh PATH；远程验证命令显式加入 `~/.cargo/bin`。

## 已执行验证

第 2 至 11 节保留阶段 0/1 的历史验证记录；当时 `lan-mouse-gtk` 仍在 workspace 中。当前桌面前端重建后的最终结果以第 12 节为准。

### 1. 格式检查

```powershell
cargo fmt --all -- --check
```

结果：通过，无输出。

### 2. 交接文档原建议命令

```powershell
cargo check --workspace --no-default-features
```

结果：失败。原因不是核心代码，而是 `--workspace` 仍包含独立成员 `lan-mouse-gtk`；根包的 `--no-default-features` 不会禁用该成员自身的 GTK 依赖。最终在 `gio-sys/gdk4-sys` 等构建脚本处因缺少 `pkg-config`、GTK4 系统库失败。

### 3. 修正后的核心检查

```powershell
cargo check --workspace --exclude lan-mouse-gtk --no-default-features --offline
```

结果：通过。

警告：

- MSVC 链接器输出“正在创建库”被 Rust 记录为 `linker_messages`；
- 无 GTK 条件编译下 `src/main.rs:121` 的 `start_service` 未使用。

首次联网检查在根包 `build.rs -> shadow-rs -> cargo metadata` 的网络请求处等待；依赖下载完成后改为 `--offline`，3.6 秒通过。后续可重复构建应优先使用锁文件与缓存，联网只用于确有缺失的依赖。

### 4. 核心测试

```powershell
cargo test --workspace --exclude lan-mouse-gtk --no-default-features --offline
```

结果：命令通过；所有 crate 与 doc-test 合计实际运行 0 个测试。

### 5. 核心 Clippy

```powershell
cargo clippy --workspace --exclude lan-mouse-gtk --all-targets --no-default-features --offline
```

结果：通过，未使用 `-D warnings`。

新增观察到的警告：

- `src/config.rs:521`：格式化参数存在冗余 `&`；
- `src/config.rs:551`：格式化参数存在冗余 `&`；
- `src/main.rs:121`：无 GTK 构建下 `start_service` 未使用。

这些警告未在阶段 0 顺手修改，避免将审计与功能改造混在一起。

### 6. 核心 release 构建

```powershell
cargo build --release --workspace --exclude lan-mouse-gtk --no-default-features --offline
```

结果：通过，用时约 2 分钟。

产物：

```text
F:/1/CrossDesk/target/release/lan-mouse.exe
size: 4,446,208 bytes
```

启动验证：

```text
lan-mouse 0.11.0
branch:main
commit_hash:392af44c
build_env:rustc 1.97.1,stable-x86_64-pc-windows-msvc
```

### 7. 阶段 1 指标关闭构建

```powershell
cargo check --workspace --exclude lan-mouse-gtk --no-default-features --offline
```

结果：通过。`metrics` 未启用时观测时间戳为零大小类型，不采集详细指标。只剩阶段 0 已记录的 linker 和无 GTK 警告。

### 8. 阶段 1 指标开启检查与测试

```powershell
cargo check --workspace --exclude lan-mouse-gtk --no-default-features --features metrics --offline
cargo test --workspace --exclude lan-mouse-gtk --no-default-features --features metrics --offline
```

结果：通过。新增并通过 2 个单元测试：

- 最近邻 P50/P95/P99 分位数计算；
- 4096 样本滚动窗口上限。

### 9. 阶段 1 Clippy

```powershell
cargo clippy --workspace --exclude lan-mouse-gtk --all-targets --no-default-features --features metrics --offline
```

结果：通过。没有新增 Clippy 警告；仍存在阶段 0 已记录的 `src/config.rs` 两条冗余借用、MSVC linker 输出和无 GTK 下 `start_service` 未使用。

### 10. 阶段 1 release 与冒烟测试

```powershell
cargo build --release -p lan-mouse --no-default-features --features metrics --offline
```

结果：通过。使用 dummy 捕获/注入后端和独立端口执行 7 秒冒烟运行，第一个报告周期正确输出 `window_s=5.0`，空样本为 `n/a`，队列深度为 0，乱序/重复标记为 `unavailable_without_sequence`。

Windows dummy 空闲 5 秒资源采样：

```text
CPU：0.000%（按 20 个逻辑处理器归一化）
Working Set 平均：8.87 MiB
Working Set 峰值：8.89 MiB
```

该结果只验证报告器空闲开销，不替代真实 Windows/macOS 双机基线。

### 11. macOS M4 核心构建与冒烟测试

源码同步到 `/Users/shishenglin1/CrossDesk-stage1`，排除 `.git`、`target`、证书和日志。关键文件 SHA-256 与 Windows 工作树一致。

执行：

```bash
cargo fmt --all -- --check
cargo check --workspace --exclude lan-mouse-gtk --no-default-features --locked
cargo check --workspace --exclude lan-mouse-gtk --no-default-features --features metrics --locked
cargo test --workspace --exclude lan-mouse-gtk --no-default-features --features metrics --locked
cargo clippy --workspace --exclude lan-mouse-gtk --all-targets --no-default-features --features metrics --locked
cargo build --release -p lan-mouse --no-default-features --features metrics --locked
```

结果：全部通过。阶段 1 的 2 个单元测试通过；Clippy 只剩阶段 0 已记录的两条 `config.rs` 冗余借用和无 GTK `start_service` 未使用。

Mac dummy 空闲 5 秒资源采样：

```text
CPU 平均：0.000%
RSS 平均：9.10 MiB
RSS 峰值：9.12 MiB
```

release 产物正确报告 `stable-aarch64-apple-darwin`，5 秒指标窗口正常输出并通过 SIGINT 优雅退出。由于同步目录不包含 `.git`，该临时构建的 branch/commit 版本字段为空；不影响核心编译和 dummy 验证，正式发布产物需要在完整 Git 工作树构建。

原生后端静默探测确认当前 TCC 首个门禁为“辅助功能”权限：macOS 捕获后端拒绝创建，注入后端拒绝后按现有逻辑回退到 dummy。待用户在图形界面为当前 release 二进制授予辅助功能与输入监控后重新探测。另发现 CLI `MacOs` 值为 `mac-os`，与配置文件的 `macos` 名称不一致；阶段 1 不顺带修改 CLI 行为。

### 12. 桌面前端最终验证

Windows 上执行：

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo check -p lan-mouse --all-targets --no-default-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release --workspace --all-features
```

结果：全部通过。实际运行 22 个测试：`crossdesk-ui` 18 个、观测性 2 个、IPC JSON 往返 2 个。前端测试覆盖四方向吸附与画布边界、方向占用及回滚、断线和超时、旧请求清理、完整设备创建、表单校验、中文页面交互以及最小窗口长页面滚动。

release 产物：

```text
target/release/crossdesk.exe  10,976,768 bytes  PE subsystem 2 (Windows GUI)
target/release/lan-mouse.exe  10,976,768 bytes  PE subsystem 3 (Windows Console)
```

真实 Windows 运行已确认主窗口、中文 UI Automation 节点和服务连接状态可见；关闭窗口后 GUI 进程继续存活，证明托盘驻留生命周期生效。真实 dummy daemon 收到 `ShutdownService` 后按顺序终止捕获、注入和 DNS。Windows 防火墙首次监听提示未代替用户确认。

### 13. 文本剪贴板同步

已实现 Windows/macOS 双向 UTF-8 文本剪贴板同步：后台线程每 350 ms 读取平台剪贴板，通过有界通道把变化交给服务；远端写入会更新去重状态，避免两端反复回传相同文本。同步默认开启，可在 CrossDesk 设置页或 `config.toml` 的 `clipboard_sync` 字段关闭。

网络协议新增 Hello 能力位和最大 16 KiB 的可变长度文本包。新端只向声明支持的对端发送剪贴板包；旧端仍可解析 Hello 的提交哈希，不会收到超出旧接收缓冲区的文本包。`lan-mouse-proto` 与 `lan-mouse-ipc` 已相应升级至 `0.4.0`。

Windows 上执行：

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release --workspace --all-features --target-dir "target/clipboard-sync-release"
```

结果：全部通过。工作区共 28 个单元测试通过，其中新增协议编解码/兼容/大小边界 4 个、IPC 往返 1 个、设置页交互 1 个；全部文档测试通过。独立 release 产物 `target/clipboard-sync-release/release/crossdesk.exe` 为 11,033,600 bytes。常规 `target/release/crossdesk.exe` 当时被两个正在运行的 CrossDesk 进程占用，因此未终止用户进程，改用独立目标目录完成完整 GUI 链接验证。

尚未执行 Windows/macOS 双机剪贴板互传实测；当前环境只有 Windows，且本地冒烟若直接写入系统剪贴板会覆盖用户内容。双机验收需覆盖纯英文、中文、多行代码、禁用开关、超过 16 KiB 的跳过行为，以及新版本连接旧版本时输入功能不受影响。

## 未执行验证

| 项目 | 原因 | 应执行环境 | 后续验证 |
|---|---|---|---|
| macOS 辅助功能与输入监控 | 原生后端已静默探测，当前缺少辅助功能权限 | Mac mini M4 | 为当前 release 二进制授权辅助功能与输入监控，然后重新探测 |
| Windows -> Mac 双机切屏 | 尚未部署两端 | 目标两台设备 | 验证右侧进入、左侧返回、release bind 和断连释放 |
| 性能目标 | 埋点已实现，尚无真实双机样本 | 目标局域网 | 记录 P50/P95/P99、队列长度、丢弃、CPU 和内存 |
| 睡眠/唤醒与网络抖动 | 尚未端到端运行 | 目标两台设备 | 断网、休眠、锁屏、切换 Wi-Fi 后检查按键状态 |
| macOS egui GUI 与 `.app` bundle | 当前会话运行于 Windows | Mac mini M4 | 执行 `cargo bundle --release --bin crossdesk`，检查菜单栏、TCC 权限、授权后重启和撤权退出 |
| Windows/macOS 双机方向切换 | 尚未部署新 GUI 到两端 | 目标两台设备 | 验证右侧进入/左侧返回，再改为左侧并验证重启持久化 |

## 环境与仓库状态

- `origin` 已配置为 `https://github.com/feschber/lan-mouse.git`；
- 分支为 `main`；
- `docs/` 当前为未跟踪内容，包含 PRD、交接文档、架构审计、实施状态与性能基线；
- `target/` 构建产物被忽略；
- 未执行 `git add`、`git commit`、`git push` 或分支操作；
- PRD 原文已归档为 `docs/PRD.md`。

## 下一步所需输入

1. Windows 显示器的分辨率、缩放比例，以及与 Mac 显示器的垂直摆放关系；
2. Mac mini 的有线与 Wi-Fi 接口中，本次基线实际使用哪一个；
3. 双机可用后按 `docs/PERFORMANCE_BASELINE.md` 完成有线或当前实际网络的 5 分钟样本；
4. 在 Mac mini 上构建 `CrossDesk.app` 并完成辅助功能、输入监控和菜单栏验收；
5. 决定正式发布前的 Windows 签名与 macOS Developer ID/公证方案。

阶段 1 在双机 P50/P95/P99、CPU、内存、事件速率和队列数据补齐前保持“进行中”；不得直接进入协议、队列或状态机改造。
