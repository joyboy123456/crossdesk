# CrossDesk 桌面构建与验收

更新时间：2026-07-26

## 产物与功能开关

默认构建包含纯 Rust `egui/eframe` 前端，不依赖 GTK4 或 libadwaita：

```text
target/release/crossdesk.exe  Windows 图形程序，无控制台窗口
target/release/crossdesk      macOS/Linux 图形程序
target/release/lan-mouse      兼容的 daemon、CLI 与测试子命令
```

构建全部默认产物：

```sh
cargo build --release --bins
```

仅构建 daemon/CLI，不引入 GUI：

```sh
cargo build --release -p lan-mouse --no-default-features
```

`lan-mouse daemon` 与 `lan-mouse cli ...` 保持兼容。`crossdesk` 和默认 feature 下无子命令的 `lan-mouse` 会启动图形控制中心。

## Windows

前置条件：Rust stable MSVC 工具链、Visual Studio 2022 Build Tools。无需 GTK、gvsbuild、MSYS2 或额外 DLL。

```powershell
cargo build --release --bins
./target/release/crossdesk.exe
```

release 的 `crossdesk.exe` 使用 Windows GUI subsystem，双击不会弹出黑色控制台。关闭主窗口后程序继续驻留通知区域；托盘菜单可重新打开窗口或选择“退出 CrossDesk”。明确退出时，GUI 会请求自己启动的 daemon 释放捕获、注入和按键状态，3 秒未结束才强制终止子进程。

## macOS

原生编译不需要 GTK。创建 `.app` 需要 `cargo-bundle`，生成现有 `.icns` 时需要 ImageMagick：

```sh
brew install imagemagick
cargo install cargo-bundle
scripts/makeicns.sh
cargo bundle --release --bin crossdesk
```

产物为：

```text
target/release/bundle/osx/CrossDesk.app
```

首次启动时授予“辅助功能”，重新启动后再确认“输入监控”。设置页会轮询权限并在需要时提供“立即重启”。运行期间撤销输入权限会触发安全退出，避免事件捕获残留。`LSUIElement` 使 bundle 以菜单栏应用运行，关闭窗口后可从菜单栏重新打开。

未签名的本地构建可能需要：

```sh
xattr -rd com.apple.quarantine "CrossDesk.app"
```

## 手工验收

Windows：

1. 双击 `CrossDesk.exe`，确认显示主窗口且没有控制台。
2. 关闭窗口，确认托盘仍在；从托盘重新打开。
3. 新增远端设备并拖到左、右、上、下空槽，重启后确认方向保留。
4. 从托盘退出，确认 daemon 结束且输入捕获/注入被释放。

macOS：

1. 启动 `CrossDesk.app`，确认菜单栏驻留和权限入口可用。
2. 授权后使用“立即重启”，确认捕获与注入状态变为已启用。
3. 撤销权限，确认 CrossDesk 安全退出。

双机：

1. Mac 配在 Windows 右侧，从 Windows 右边缘进入 Mac，并从 Mac 左边缘返回。
2. 将 Mac 改到 Windows 左侧，确认方向立即反转。
3. 重启两端，确认方向、授权和设备配置仍保留。
