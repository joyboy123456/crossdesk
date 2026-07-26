# CrossDesk 阶段 1 性能基线

更新时间：2026-07-26  
基线提交：`392af44cbe06a7a591db86856ad6ed2aa83958d8`  
状态：观测代码已实现；Windows 与 Mac 双机实测待执行

## 1. 基线边界

阶段 1 只增加观测，不修改协议编码、队列容量、事件丢弃策略、切屏判定或输入语义。详细指标由根包的 `metrics` feature 控制，默认构建不启用该 feature。

采样窗口固定保留最近 4096 个样本，避免观测数据无限增长。日志只包含事件类别、计数、耗时和队列深度，不记录实际键值、输入字符、密钥或设备指纹。

## 2. 已实现指标

| 指标 | 定义 | 当前限制 |
|---|---|---|
| RTT P50/P95/P99 | 本端发送现有 `Ping` 后，到收到对应 FIFO `Pong` 的时间 | 现有协议没有 ping ID；适合局域网基线，不用于识别乱序 Pong |
| 序列化耗时 | `ProtoEvent` 转换为线格式缓冲区的本机耗时 | 包含所有发送尝试，单位为微秒 |
| 捕获分发到发送耗时 | `InputCapture::next()` 交付事件后，到 DTLS `send` 成功的本机耗时 | Windows/macOS 后端队列等待时间单独由捕获队列指标反映 |
| 接收到注入耗时 | DTLS `recv` 返回后，到 `InputEmulation::consume` 返回的本机耗时 | 包含接收端本地排队、首次 handle 创建和后端注入调用 |
| 切屏确认耗时 | 本端收到 `CaptureEvent::Begin` 后，到远端 `Ack` 的耗时 | 不等同于完整视觉感知延迟 |
| 事件速率 | 每 5 秒窗口内成功发送、接收的输入事件数量/秒 | DEBUG 日志按 Motion、Button、Scroll、Key、Modifiers 分类 |
| 捕获队列 | Windows 容量 10、macOS 容量 32 的当前/峰值深度 | 生产者线程每 5 秒输出；关闭 feature 后无额外统计 |
| 注入队列 | 接收分发到注入任务的当前/峰值深度 | 现有队列行为未修改 |
| Motion 合并 | 当前固定为 0 | 阶段 3 才实现合并策略 |
| Motion 丢弃 | Windows 捕获队列满时按事件类别计数 | macOS 当前使用阻塞发送，不存在同类满队列丢弃 |
| 乱序/重复 | 日志输出 `unavailable_without_sequence` | 协议尚无序号，阶段 1 不应伪造为 0；阶段 3 增加兼容协议能力后测量 |
| 状态机转换 | `crossdesk::state` DEBUG 日志记录捕获状态、原因和 client handle | 不改变现有 `WaitingForAck` / `Sending` 行为 |

## 3. 启用方式

Windows 无 GTK 核心构建：

```powershell
cargo build --release -p lan-mouse --no-default-features --features metrics --offline
$env:LAN_MOUSE_LOG_LEVEL = "info,crossdesk::metrics=debug,crossdesk::state=debug"
./target/release/lan-mouse.exe daemon
```

macOS 无 GTK 核心构建：

```bash
cargo build --release -p lan-mouse --no-default-features --features metrics
LAN_MOUSE_LOG_LEVEL='info,crossdesk::metrics=debug,crossdesk::state=debug' \
  ./target/release/lan-mouse daemon
```

删除 `--features metrics` 并重新构建即可编译期关闭详细指标。默认 feature 集也不包含 `metrics`。

## 4. 测试硬件与软件

| 项目 | Windows 发送端 | Mac 接收端 |
|---|---|---|
| 设备 | 自组 PC | Mac mini M4 |
| CPU | Intel Core i5-14600KF，20 逻辑处理器 | Apple M4，10 核（4 性能核 + 6 能效核） |
| 内存 | 31.8 GiB | 16 GiB |
| 系统 | Windows 11 专业版 10.0.26200，Build 26200 | macOS 26.3.1，Build 25D2128 |
| Rust | rustc/cargo 1.97.1，MSVC | rustc/cargo 1.97.1，`aarch64-apple-darwin` |
| 构建模式 | `release` + `metrics`，无 GTK | `release` + `metrics`，无 GTK |
| 显示器 | 分辨率、缩放与垂直位置待确认 | HP27QI，2560×1440，100 Hz，无镜像 |

## 5. 网络环境

两端位于同一 `192.168.0.0/24` 局域网；Tailscale 仅作为 SSH 管理通道，已确认通过物理 LAN 直连而非 DERP 中继。CrossDesk 正式样本仍需确认使用 Mac 的有线或 Wi-Fi 接口。双机测试时每轮必须记录：

- 有线或 Wi-Fi；
- 网卡链路速率与 Wi-Fi 频段；
- 两端 IP 所属网段，但诊断归档中隐藏完整地址；
- 是否存在 VPN、代理或省电模式；
- 测试期间的基本丢包与系统负载。

下载代理 `http://127.0.0.1:7890` 只用于依赖获取，不属于输入链路，也不得计入局域网 RTT。

## 6. 测试方法

1. 在 Mac 本机编译启用 `metrics` 的相同 CrossDesk 源码，记录双方版本和构建参数。
2. 授予 macOS 辅助功能与输入监控权限，完成首次授权和 Windows 右侧进入、Mac 左侧返回。
3. 空闲运行 5 分钟，记录心跳 RTT、CPU、内存和空闲网络流量。
4. 连续移动鼠标 5 分钟，至少包含水平、垂直、斜向和快速往返；记录事件速率、队列峰值、Motion 丢弃及各耗时分位数。
5. 完成至少 100 次双向切屏，记录切屏确认 P50/P95/P99 和失败次数。
6. 执行点击、滚轮、组合键和拖拽样本；日志只核对事件类别及状态，不记录具体键值。
7. 有线和稳定 5/6 GHz Wi-Fi 分开采样，不能合并为同一组分位数。
8. 使用系统任务管理器或 Activity Monitor 记录进程 CPU 与内存；同一轮注明采样周期和峰值/平均值。

## 7. 当前测量结果

Windows 与 Mac 均已完成无 GTK 指标构建和 dummy 冒烟，但尚无真实输入链路数据。本节保留真实测量位置，不以本机函数耗时冒充跨设备端到端延迟。

观测代码冒烟测试使用 `release + metrics`、dummy 捕获/注入后端和独立端口运行。Windows 5 秒内按 1 秒间隔采集 5 个 Working Set 样本：平均 8.87 MiB，峰值 8.89 MiB；按 20 个逻辑处理器归一化后的 CPU 采样为 0.000%。Mac 同样采集 5 个 RSS/CPU 样本：平均 9.10 MiB，峰值 9.12 MiB，CPU 平均 0.000%。这些结果只证明空闲报告器没有明显资源增长，不代表真实后端、GTK 或双机负载。

| 场景 | 样本 | P50 | P95 | P99 | CPU | 内存 | 结果 |
|---|---:|---:|---:|---:|---:|---:|---|
| 有线 RTT | 0 | 待测 | 待测 | 待测 | 待测 | 待测 | 未执行双机测试 |
| 有线鼠标接收至注入 | 0 | 待测 | 待测 | 待测 | 待测 | 待测 | 未执行双机测试 |
| 有线切屏确认 | 0 | 待测 | 待测 | 待测 | 待测 | 待测 | 未执行双机测试 |
| Wi-Fi RTT | 0 | 待测 | 待测 | 待测 | 待测 | 待测 | 未执行双机测试 |
| Wi-Fi 鼠标接收至注入 | 0 | 待测 | 待测 | 待测 | 待测 | 待测 | 未执行双机测试 |

Windows 与 Mac 的 dummy 冒烟均在第一个 5 秒周期正确输出 `window_s=5.0`、空样本 `n/a`、零队列深度和 `ordering=unavailable_without_sequence`。两端测试都由验证脚本受控终止，不是崩溃。

## 8. 已知瓶颈

1. Windows Motion、键盘和按钮共用容量 10 的捕获队列；阶段 1 只统计其深度与分类丢弃，不改变策略。
2. 当前协议没有序号、会话 ID 和采集时间戳，因此无法可靠统计乱序、重复或跨设备单向延迟。
3. Motion 尚未合并或淘汰过期事件，网络抖动时可能播放历史轨迹。
4. Ping 没有请求 ID；RTT 使用同一 DTLS 连接内的发送/响应 FIFO 近似匹配。
5. Windows `SendInput` 失败重试和 macOS 可恢复路径 `unwrap()` 仍是后续阶段风险，本阶段不混入修复。
6. macOS CLI 的后端值由 Clap 生成为 `mac-os`，但配置序列化名称为 `macos`；阶段 1 原生探测通过自动选择后端绕开，后续配置体验阶段应统一名称。

## 9. 阶段 1 完成条件

阶段 1 在以下证据补齐后才能标记完成：

- Windows 与 Mac 使用同一 CrossDesk 指标构建完成真实双机采样；
- 有线或当前实际网络至少完成一组 5 分钟输入样本；
- RTT、接收至注入、切屏确认均产生 P50/P95/P99；
- CPU、内存、队列峰值、事件速率和分类丢弃有真实记录；
- 两端编译或运行限制已记录；
- 根据数据确认阶段 2、3 的优先改动，不在阶段 1 提前优化。
