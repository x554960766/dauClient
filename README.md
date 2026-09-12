# Umeng DAU Client (友盟活跃与留存自动化客户端)

[![Tauri v2](https://img.shields.io/badge/Tauri-v2-blue.svg)](https://tauri.app/)
[![Vue 3](https://img.shields.io/badge/Vue-3.x-emerald.svg)](https://vuejs.org/)
[![Rust](https://img.shields.io/badge/Rust-2021_edition-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20macOS-lightgrey.svg)](#)

友盟 DAU 批量执行与留存模拟客户端是一款基于 **Tauri v2 + Vue 3 + Rust** 构建的高性能桌面端应用。专为移动应用测试团队、数据指标验证与合规性评估打造，提供 Android 模拟器自动化集群调度、轻量级设备快照堆栈轮换、真实手机 USB 飞行模式公网换 IP 等一整套全链路解决方案。

---

## ✨ 核心特性

### 1. 动态档案堆栈与防重留存轮换系统 (Profile Stack & Archive)
- **微型设备快照档案（~2 KB/台）**：通过备份设备的 ANDROID_ID (SSAID)、App 私有 `shared_prefs` XML（如 `umeng_general.xml`）以及硬件品牌型号属性，仅需数兆磁盘即可轻松管理 300~10,000+ 虚拟设备的完整身份凭证。
- **FIFO 动态容量池与自然衰减**：支持配置堆栈容量（默认 300 台），新设备执行后入栈；满仓后采用概率淘汰旧设备，拟合真实用户的生命周期自然轮换。
- **单日防重与智能调度**：档案打上当天使用日期锁（北京时间），每日每台设备仅生效一次，防止重复回访失真；每天自动检查重置状态，亦支持一键手动解除限制。
- **双模分配机制**：
  - **自动混合轮换**：30%~50% 走 L3 恢复出厂生成全新增设备，50%~70% 抽取堆栈中未使用的历史档案还原为老客留存回访。
  - **单项定向模式**：支持“强制留存测试”与“强制全新增测试”，便于专项验证。

### 2. 真实手机 USB 飞行模式自动换 IP (Rotate IP via USB/ADB)
- **波次间隙自动换 IP**：支持识别 USB 插入的真实 Android 物理手机，在各测试波次间隙自动切换飞行模式断开并重连基站，获取全新蜂窝公网出口 IP。
- **USB 共享自愈守护**：自动检测与恢复因网络切换而被 Android 系统意外关闭的“USB 网络共享”状态。
- **公网 IP 校验比对**：自动请求出口 IP 节点比对前后变化，防止同一 IP 频段聚集与关联风控。

### 3. 多级重置阶梯与深度设备指纹拟真
- **4 级环境重置阶梯**：
  - **L1 · `pm clear`**：极速清理应用私有数据（~3s/台）。
  - **L2 · 卸载重装**：重置应用安装上下文与签名状态（~15s/台）。
  - **L2.5 · 多用户空间**：创建独立 Android User 空间并隔离权限（~15s/台）。
  - **L3 · 恢复出厂**：抹除模拟器用户镜像重置 SSAID 与设备硬件（~100s/台，权威真实新增）。
- **多维硬件指纹伪装**：自动配置品牌、型号、主板、处理器架构、系统指纹、屏幕分辨率/DPI，注入 Wi-Fi/蜂窝网络状态与移动网络 SIM 运营商信息。

### 4. 自动化 UI 引导与冷启动保活
- **UI 自动化接管**：自动寻找并点击隐私协议“同意并继续”弹窗，智能穿透启动引导页。
- **启动停留与上报保活**：支持配置应用冷启动驻留时长（Dwell Time）与前台等待周期，保障友盟 SDK 充分完成初始化与心跳事件上报。

### 5. 安全合规与闭环对账
- **APK 静态安全校验**：拖拽 APK 自动解析包名、版本、声明 AppKey 一致性，拦截 Debuggable 不规范包与合规黑名单。
- **零证书本地透明代理**：内嵌无解密代理服务器，仅记录 CONNECT 请求主机与数据包计数，生成不可伪造的通信硬证据。
- **防跨零点机制**：北京时间跨零点自动拦截与预估保护，避免批次数据跨天污染。

---

## 🏗️ 架构概览

```
umeng-dau-client/
├── src/                        # 前端源码 (Vue 3 + TypeScript + Naive UI + Pinia)
│   ├── api/                    # IPC 通信层与后端事件监听 (events.ts / ipc.ts)
│   ├── components/             # 通用组件 (ApkDropZone 等)
│   ├── pages/                  # 业务页面 (SetupWizard, Preflight, Pilot, Batch, Report)
│   └── stores/                 # 状态管理 (app, batch, setup)
├── src-tauri/                  # 后端源码 (Rust 异步运行时 + Tauri v2)
│   ├── src/
│   │   ├── adb/                # ADB 协议封装、USB 真机换 IP、设备管理
│   │   ├── avd/                # Android 虚拟机创建、删除、参数调优
│   │   ├── commands/           # 暴露给前端的 Tauri IPC 命令
│   │   ├── engine/             # 重置阶梯、设备池调度、档案管理 (profile_archive/stack)
│   │   ├── pipeline/           # 预检、试点调测与批量放量流水线
│   │   ├── proxy/              # 轻量级 TCP 透明代理与 CONNECT 审计
│   │   ├── report/             # 运行报告聚合与 Markdown 导出
│   │   ├── sdkmgr/             # Android SDK 自动部署与依赖检查
│   │   └── uiautomation/       # UI 自动化、节点倾倒、协议弹窗自动化
│   └── tauri.conf.json         # 桌面端配置与多平台打包目标
└── scripts/                    # 依赖预热与离线 Bundle 制作脚本
```

---

## 🚀 快速上手

### 环境依赖
- **Node.js**: >= 18.0.0
- **Rust**: >= 1.75.0 (含 `cargo`)
- **Android SDK / Emulator**: 可通过应用内「初始化向导」一键检测与自动安装。

### 本地开发

1. **克隆项目并安装依赖**
   ```bash
   git clone https://github.com/x554960766/dauClient.git
   cd dauClient
   npm install
   ```

2. **启动本地开发桌面应用**
   ```bash
   npm run tauri dev
   ```

### 生产打包构建

- **macOS 构建 (.dmg / .app)**:
  ```bash
  npm run tauri build
  ```
  产物位于 `src-tauri/target/release/bundle/dmg/`。

- **Windows 构建 (.exe 安装包)**:
  ```bash
  npm run tauri build -- --target x86_64-pc-windows-msvc
  ```
  产物位于 `src-tauri/target/release/bundle/nsis/`。

---

## 📖 操作流程

1. **初始化向导 (Setup Wizard)**
   - 首次启动自动检测 Android SDK、cmdline-tools、JRE 环境以及系统硬件虚拟化支持状态。
   - 支持自动下载官方套件或链接现有 SDK 目录。
2. **环境预检 (Preflight)**
   - 自动执行 7 项环境健康度检查（端口占用、磁盘空间、AVD 配置、网络连通性等）。
3. **单机试点 (Pilot)**
   - 单台设备快速拉起，实机调试 UI 自动化穿透逻辑与友盟 SDK CONNECT 上报状态。
4. **批量放量 (Batch Execution)**
   - 拖拽待测 APK 文件，系统自动解析包信息。
   - 自由设定并发设备数、目标执行总量、留存/新增分配模式。
   - 如有需要，开启“手机 USB 换 IP”功能，选择物理手机即可实现全自动无人值守轮换。
5. **数据对账与报告 (Report)**
   - 查看每台设备的 ANDROID_ID、UMID 变化轨迹、执行耗时与网络请求计数。
   - 一键导出标准 Markdown / JSON 批次审计报告。

---

## 🔒 隐私与合规说明

- 本工具所有操作均在使用者本地计算机与私有模拟器网络内完成，不设任何中心化数据上报。
- 内置透明代理不解密 HTTPS 报文内容、不植入自签名根证书，仅对 CONNECT 请求的主机及流量包进行计数与审计。

---

## 📄 开源许可证

本项目基于 MIT License 协议开源。
