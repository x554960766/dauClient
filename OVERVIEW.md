# 友盟 DAU 批量执行客户端 — 实现总览

> 依据 `umeng-android-emulator-client-design.md` v1.1 实现。2026-08-06 完成 M1–M4 全部代码，编译与前端构建双绿。

## 已完成

**Rust 核心（src-tauri/src/，cargo check 通过）**

| 模块 | 内容 |
|---|---|
| `sdkmgr/` | 组件清单检测、cmdline-tools/JRE 官方直链下载（断点续传+进度事件）、sdkmanager 安装 platform-tools/emulator/build-tools/系统镜像、license hash 直写（P0-2）、Windows 短路径默认（P0-3a）、虚拟化检测 |
| `adb/` | 类型化封装全部 adb 子命令；**私有 adb server 环境注入**（`ANDROID_ADB_SERVER_PORT`/`ANDROID_SDK_HOME`/`ANDROID_AVD_HOME`/`JAVA_HOME`，P0-1，与 Android Studio 并存）；wait_boot/wait_net；多用户 create/start/remove；run-as 标识读取 |
| `avd/` | avdmanager create/delete/list；`Emulator::boot` 带 `current_dir` 修正（P1-5）、`-http-proxy` 挂载、`fw.max_users` prop |
| `engine/` | 重置阶梯 L1/L2/L2.5(多用户)/L3、槽位分片并发（上限 8）、首台跳过重置、AvdRegistry 落盘清理（防强杀留孤儿）、跨零点预估（北京时间） |
| `proxy/` | 内嵌 TCP 代理：CONNECT host 计数 + 透传，不解密零证书（P1-2 硬证据） |
| `identity/` | ANDROID_ID/UMID 提取（run-as 优先、root 兜底），双标识对比判重置生效 |
| `pipeline/` | Phase 0 预检 7 项、Phase 1 试点状态机（含重置试验 + GATE 1 分支判定）、Phase 2 槽位 worker、逐台标识落盘 |
| `report/` | result.json、Markdown 报告（验收表留空待填）、T+1 逐台差集对账（P1-3） |
| `commands/` | 14 个 IPC 命令全量实现 |

**前端（src/，vite build + vue-tsc 通过）**

- SetupWizardPage：组件清单 + 下载进度 + 虚拟化状态
- PreflightPage：7 项预检卡片 + 跨零点告警 + 残留清理
- PilotPage：状态机步骤条 + 代理 CONNECT 计数 + ANDROID_ID/UMID 一键复制 + 重置阶梯标定（L1/L2/L2.5/L3 逐级试验）+ logcat 辅助流 + GATE 1 人工确认
- BatchPage：APK 拖入自动解析（包名/AppKey 一致性/debuggable/黑名单）、耗时预估、合规勾选、圆形进度 + 设备网格看板
- ReportPage：历史批次、Markdown 导出、T+1 对账（过滤率归因）

## 运行方式

```bash
cd umeng-dau-client
npm install          # 已执行
npm run tauri dev    # 开发模式
npm run tauri build  # 打包 .dmg / .exe
```

## 遗留项（对应设计文档里程碑）

- M3 待实测假设 #1–#4（多用户 UMID 变化、SDK 走代理、run-as 可读、-prop 生效时机）需真机跑试点验证
- M5 Windows 适配：AEHD 引导安装流程目前是检测+文案，静默安装未实现；`wmic` 内存检测可换 PowerShell CIM
- 合规黑名单需手工在 settings.json 配 `prod_appkey_blacklist` 数组
- 图标为占位绿色方块，正式分发需替换并做签名/公证
