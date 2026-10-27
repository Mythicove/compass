# Mythic Compass

[English](./README.md) | **简体中文**

Mythicove 生态的 Minecraft 启动器（Tauri 2 + React 前端，Rust 核心）：用玩家的**启动器独立密码**登录 `mythic-mirror`、skinview3d 皮肤预览、从 `mythic-hoard` 浏览模组。定位私群/小社区场景，不是公共市场竞争者。

## 现状

开发按阶段计划推进：[Compass 启动器开发计划](https://docs.gov.skin/plans/compass-launcher/)（P0 → P6，目标是三平台上达到 HMCL/PCL 级能力）；进度见[当前进度页](https://docs.gov.skin/plans/status/)。

**目前还不能启动 Minecraft。** 原生启动所需的 Rust 核心已经落地并用真实数据做了契约测试，缺的是围绕它的命令层与 UI 接线（建实例 → 安装 → 启动 → 日志）。

已实现：

- **P0 —— 骨架与仓库卫生**
  - **所有后端 HTTP 都在 Rust 核心里**（`src-tauri/src/api.rs`）。webview 完全不碰网络，于是桌面端既不依赖 Mirror 的 CORS 配置，CSP 也不需要放行任何远端 `connect-src`/`img-src`——皮肤由 Rust 取回后以 `data:` URL 交给前端。
  - **只有具名命令。** 原先的 `launch(program, args)`（任意命令执行）与 `download_file(url, dest)`（任意 URL 写任意路径）已删除，也不提供通用转发命令。
  - **令牌不进 webview**：命令返回的是不含令牌的账号视图；`accounts.json` 留在数据目录（P4 迁到系统钥匙串）。
  - 登录用**玩家名 + 该玩家的启动器独立密码**（[ADR-0012](https://docs.gov.skin/architecture/adr-0012-credential-separation/)），不是网站主密码。
  - 持久化设置（`compass.json`，带输入收敛与原子写）、明确的数据目录布局（资源/库/版本全局共享，实例互相隔离）、深浅色与跟随系统主题、中英双语界面。
  - 三平台 CI（`cargo fmt --check`、`clippy -D warnings`、`cargo test`、typecheck、前端构建）+ 按需打包成 draft release。
- **P1（进行中）—— 原生启动的核心，全在 Rust，且对着真实数据做契约测试**
  - `version.rs`：版本 JSON 解析、平台规则求值（`os.name`/`os.arch`/`os.version` 正则/`features`，最后一条命中的规则决定）、Maven 坐标 → 路径、库解析（老式 `natives`+`extract` 与现代 4 段坐标走 classpath 两种形态）、`arguments`（1.13+）与 `minecraftArguments`（≤1.12）两套启动参数构造、版本继承合并。
  - `net.rs`：唯一的下载实现——并发、退避重试、sha1 与大小双校验、`Range` 断点续传、按主机映射的镜像改写（BMCLAPI 兼容）并在镜像失败时回退官方源、校验通过才把 `.part` 落成正式文件。
  - `assets.rs` / `meta.rs` / `install.rs`：内容寻址的资源对象（按 hash 去重）、1.8 之前的 `virtual/` 布局、版本清单与继承链解析、幂等安装（全部落进共享缓存）。
  - `java.rs`：靠真跑 `java -version` 探测运行时（绝不信路径名或 `JAVA_HOME`），候选有明确优先级，老版本可用 `exact` 模式锁定 Java 8。
  - `launch.rs`：JVM 参数 → 主类 → 游戏参数；1.7–1.18.1 的 Log4Shell 缓解；那个**不在** `arguments.jvm` 里的 `logging.client.argument`；老版本 macOS 的 `-XstartOnFirstThread`；带 zip-slip 防护的 natives 解压；同时读 stdout 与 stderr 的日志转发。
  - `instance.rs`：每实例一个 `instance.json` + 独立 `.minecraft`，slug 校验挡住路径穿越。

尚未实现（按阶段）：建实例/安装/启动的接线与进度 UI（**P1 剩余**）、实例与 mod/资源包管理（**P2**）、mod loader 与整合包（**P3**）、多账号与钥匙串存储（**P4**）、崩溃诊断、Java 自动下载与自动更新（**P5**）、三平台签名打包与发布（**P6**）。

原先"把 `.mrpack` 交给 Prism Launcher"的路径是**直接删掉**而不是与新路径并存——只留一条启动路径。

## 运行

```bash
pnpm install
pnpm typecheck      # tsc --noEmit
pnpm tauri dev      # 开发
pnpm tauri build    # 打包
```

服务器地址默认 `http://localhost:8080`，可在「设置」里修改（持久化到数据目录的 `compass.json`，设置页会列出完整目录布局）。

## 测试

```bash
cd src-tauri
cargo test                                   # 81 个单元/契约测试，离线可跑
cargo test --test install_smoke -- --ignored # 真下载约 550MB 装一个 1.21.4（需联网）
```

解析与参数构造都是对着**真实版本 JSON** 测的，样本已入库（`src-tauri/tests/fixtures/`：1.7.10、1.12.2、1.16.5、1.20.1、1.21.4，一个真实的 Fabric 继承型 profile，以及裁剪过的资源索引与版本清单）。手写假样本会把"我以为的格式"当成契约。

## 目录

```
src-tauri/src/
  paths.rs      数据目录布局（全局缓存 vs 实例隔离）
  config.rs     compass.json：默认值、输入收敛、原子写
  account.rs    已登录账号（令牌只留在这里，不跨进 webview）
  api.rs        Mirror/Hoard 的具名 HTTP 客户端（唯一访问我们服务端的地方）
  version.rs    版本 JSON：规则、库、参数、继承合并
  net.rs        下载器（并发、重试、校验、续传、镜像）
  assets.rs     资源索引：内容寻址对象、1.8 之前的 virtual 布局
  meta.rs       版本清单与元数据获取
  install.rs    安装计划与编排（落进共享缓存）
  java.rs       Java 运行时探测（实测，不靠猜）
  launch.rs     命令行构造、natives 解压、进程拉起、日志转发
  instance.rs   实例模型（instance.json + 独立 .minecraft）
  error.rs      错误序列化成 { code, message }，前端按 code 本地化
  commands.rs   Tauri 命令（薄层）
  state.rs      共享状态
src/
  i18n/         中英两份文案表
  lib/ipc.ts    与 Rust 命令一一对应的类型化封装
  views/        首页 / 实例 / 模组 / 设置
```

## 许可证

[Elastic License 2.0 (ELv2)](./LICENSE)

本仓库曾以 AGPL-3.0 开发（未发布过 tag），2026-10-26 起换证为 ELv2；历史与影响见 [LICENSE.history](./LICENSE.history)。
