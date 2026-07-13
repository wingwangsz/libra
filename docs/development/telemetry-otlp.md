# OTLP 遥测（lore.md 1.7）

## 目标与硬约束

feature-gated OTLP trace 导出，**默认二进制零影响**（lore.md:293「feature
gating 必须严格」）。隐私允许清单（lore.md:725）：只导出操作名 / 时长 /
失败稳定码；禁止远端 URL、令牌、绝对路径、ref 名、用户身份；collector
端点必须用户显式配置；off = 什么都不离开本机。

## 设计

- **结构性允许清单**（非逐 span 清洗）：只有目标 `libra::telemetry` 被导出
  ——OTLP 层挂 `Targets` 每层过滤器，代码库里其它任何 span/event 无论携带
  什么都出不去。v1 唯一的 span 在 `cli.rs::parse()` 包住整个调度：
  `libra.command`（canonical 子命令名，argv token 经 clap 元数据解析——
  别名 `br`→`branch`，绝不取用户 argv 内容）+ 时长 + 失败时
  `otel.status_code=ERROR` 与 `libra.error_code=LBR-*`。Resource 用**空
  builder**显式给 `service.name`/`service.version`（默认 builder 会吸
  OTEL_RESOURCE_ATTRIBUTES——清单违规）。
- **门控**：编译 `--features otlp` **且** `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
  （或通用 `OTEL_EXPORTER_OTLP_ENDPOINT`）已设 **且** `OTEL_SDK_DISABLED`
  非 true。无默认端点。端点须 https（loopback 允许 http——本地 collector）。
- **传输**：http-proto + blocking reqwest + rustls。**不用 tonic/gRPC**：
  init 与 flush 跑在无 runtime 的 main 线程上（tokio runtime 生灭于 32MiB
  `libra-cli` 工作线程内）——这是唯一站得住的理由（tonic 0.14 已在默认图里
  经 libvault 统一，版本斜错不是理由）。**约束**：telemetry 初始化/关停
  不得移入 parse_async/exec_async（那里有 runtime；blocking reqwest 会
  panic）。
- **fmt 输出字节不变**：span 无条件发射（普通 tracing，无订阅者即 no-op），
  但 fmt 层的每层过滤器**排除** `libra::telemetry` 目标——否则
  `LIBRA_LOG=libra=debug` 用户的每行日志都会多出 span 作用域前缀。
- **生命周期**：main() 以 scopeguard 在两条退出路径（成功/错误 exit）前
  flush（SDK 有界超时；失败仅警告，绝不改变命令结果）。**已知限制**：约
  21 个 plumbing 命令在调度内 `std::process::exit`（apply/cat-file/fsck/
  diff-tree 族/update-ref/…）——它们跳过 flush，span 丢失。可观测性用途下
  可接受；逐命令改造为返回退出码是后续项。库内嵌者（exec/exec_async）不经
  main()，无遥测（文档化）。
- **中立性证据**（字节同一性不可证——registry 重构与未门控 span 编译进默认
  二进制）：(a) `cargo tree --edges normal | grep -i opentelemetry` 默认构建
  为空；(b) 常驻 compat guard `compat_otlp_feature_gate_guard` 文本钉住
  default 不含 otlp、四依赖 optional、模块/main.rs 使用点 cfg 门控；
  (c) 行为回归（logfile 套件 + LIBRA_LOG 格式平价）。
- **CI**：compat-clippy 跑 `--all-features`（otlp 代码天然被 lint）；
  compat-offline-core 增加一行 `cargo test --features otlp --test
  otlp_telemetry`（wire test：mock collector 收到导出、含 vetted span、
  无路径泄漏）；compat-redundancy 不受影响（依赖不 vendor，任务在
  third-party/ 缺失时提前成功）。

## 延后（有因）

metrics / logs 导出、内部子 span（须逐个审计为纯静态串后才可加入允许
清单）、采样旋钮、gRPC 传输、process::exit 族的 flush 改造、
`OTEL_EXPORTER_OTLP_HEADERS` 认证头。注：opentelemetry-otlp 的 http-proto
传递启用 metrics API 代码（编译但未使用）。

## 维护要求

任何新导出属性必须先对照 lore.md:725 清单审计为静态内容；`libra::telemetry`
目标之外的 span 永不放行；main.rs 的 cfg 门控受 compat guard 保护。
