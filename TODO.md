# LunaMate 开发计划

本文档用于记录 Live2D 桌宠能力的实施顺序、验收条件和后续工作。完成项应在对应
检查框中标记，发现的新工作应放入最接近的阶段，避免在当前阶段无边界扩展范围。

## 当前里程碑：HitArea 与点击动作

目标数据流：

```text
GPUI 点击
  -> 当前显示帧的 HitArea 命中检测
  -> 有界 ModelCommand 队列
  -> 后台 AnimatedModel
  -> 部位到 motion group 的约定映射
  -> 一次性动作
  -> 动作完成后恢复 Idle
```

当前里程碑不包含系统级透明区域点击穿透、拖拽手势、聊天联动和表情资源编辑。

## GPU underlay

- [x] Windows 使用同一 HWND 的 non-topmost DirectComposition target。
- [x] macOS 使用位于 GPUI view 下方的 sibling `NSView` 与 `CAMetalLayer`。
- [x] Wayland 使用 `wl_subsurface.place_below(parent)` 和独立 Vulkan surface。
- [x] X11 及 GPU 初始化、透明 Alpha、surface/device 失败时回退 CPU renderer。
- [x] 使用 Mocari WGPU renderer 处理网格、绘制顺序、蒙版和混合模式。
- [x] 将 Mocari 0.4.0 源码与 MIT 许可证纳入 `vendor/mocari`，记录上游 commit 和本地差异。
- [x] 将 Mocari backend 适配到 `gpui_wgpu::wgpu` 29，移除 WGPU 30 及其 core、HAL 和 Naga 依赖副本。
- [x] GPU 帧成功 present 后再发布对应 HitArea，帧通知使用有界合并通道。
- [x] 窗口关闭前停止 GPU worker，并按 surface、原生 attachment 的顺序释放资源。
- [ ] 为 GPUI 官方 renderer 设计可排序的外部绘制接口；验收条件为 Windows、macOS 和 Linux 均可复用 GPUI device/queue 并在同一帧提交 Live2D mask/model pass。
- [ ] 在真实 Windows 10/11 上验证透明边缘、控件层级、resize、DPI 和 device loss。
- [ ] 在真实 macOS 上验证 sibling view、PostMultiplied Alpha 和窗口关闭顺序。
- [ ] 在 GNOME/KDE Wayland 上验证 subsurface 层级、fractional scale 和输入区域。
- [ ] 使用包含反向蒙版、多蒙版及三种 blend mode 的模型做 CPU/GPU 像素验收。

### 模块框架

- [x] 建立模型能力模块，集中保存已解析的 HitArea 及后续能力诊断。
- [x] 建立交互命令模块，隔离 GPUI、Live2D runtime 和动作控制器。
- [x] 建立 HitArea 帧快照模块，保存与当前图像一致的命中区域。
- [x] 建立动作反应解析模块，集中处理 `Tap@Body` 等命名约定。
- [x] 将 crate 根收敛为 `agent`、`app`、`config`、`database`、`logging`、`model`、
  `platform` 和 `ui`，由各顶级模块提供稳定 façade。
- [x] Agent 对外隐藏 session、store、Provider adapter 和配置草稿，只暴露对话、设置视图
  与关闭接口。
- [x] 将模型运行时、资源、交互、帧调度和 CPU/GPU 渲染收口到 `model`，原生窗口与
  underlay attachment 收口到 `platform`。
- [x] 将配置领域与 GPUI 设置视图分离，并把可持久化外观类型保留在配置域。
- [x] 将各顶级模块自身及子模块测试集中到对应 `tests/mod.rs` 与 `tests/xxx.rs`。

### HitArea

- [x] 从 `.model3.json` 读取模型声明的 `HitAreas`。
- [x] 将 HitArea ID 解析为 Mocari Drawable 索引。
- [x] 对无法解析的 HitArea 产生可诊断警告，但不阻止模型渲染。
- [x] 在 motion、expression、physics 和 pose 更新完成后读取当前 Drawable 顶点。
- [x] 使用与图像相同的 `ModelTransform` 生成光栅坐标包围盒。
- [x] 将图像、光栅尺寸和 HitArea 包围盒作为一个不可变帧快照发布。
- [x] 按模型声明顺序处理重叠区域，与 Mocari 当前包围盒语义保持一致。
- [x] 跳过空顶点、非有限坐标和无法解析的命中区域。

### 点击坐标

- [x] 将窗口逻辑坐标转换到 `ObjectFit::Contain` 的实际图像绘制区域。
- [x] 将图像绘制区域坐标转换为离屏光栅坐标。
- [x] 正确处理 HiDPI、1.25 倍超采样、1280 像素上限和尺寸取整。
- [x] 点击图像留白、透明窗口区域或 HitArea 外部时不触发动作。
- [x] 模型图像使用按模型 generation 区分、跨动画帧稳定的 GPUI 元素 ID。
- [x] 设置面板打开时禁用模型点击。
- [x] 设置、关闭和移动按钮不得触发模型点击。

### 模型命令通路

- [x] 使用有界、非阻塞队列传递离散模型命令。
- [x] 鼠标视线继续使用只保留最新值的共享状态，不混入命令队列。
- [x] 每次加载模型创建新的命令发送端和接收端。
- [x] 模型切换后丢弃旧 generation 的命令和帧。
- [x] 每帧只消费有限数量的命令，避免输入积压影响渲染。
- [x] GPUI 回调不得等待 CPU 光栅化或直接锁住 `AnimatedModel`。

### 点击动作

- [x] 按 `Tap@{Name}`、`Tap{Name}`、ID 回退和 `Tap` 的顺序查找动作组。
- [x] 点击动作强制按一次性动作播放，不信任资源中的 `Loop` 值。
- [x] 一次性动作结束后自动恢复 `Idle`。
- [x] 多个 Idle 或点击 clip 使用确定性的轮换策略。
- [x] 建立 `Idle < Interaction` 的基础动作优先级。
- [x] 同级交互动作允许替换，Idle 不得打断交互动作。
- [x] 非法或零持续时间动作不得永久占用当前播放状态。
- [x] 每帧同时重置参数和 PartOpacity 覆盖，防止部件透明度残留。
- [x] 对命中动作增加短冷却，避免一次点击风暴连续重启动作。
- [x] 找不到匹配动作时保持当前状态，不把它视为模型渲染错误。

### 当前里程碑测试

- [x] 测试 `ObjectFit::Contain` 的横向和纵向留白坐标转换。
- [x] 测试窗口点到光栅点的边界、非法尺寸和矩形目标。
- [x] 测试 HitArea 包围盒内部、边界和外部命中。
- [x] 测试点击动作组候选顺序和去重。
- [x] 测试一次性动作忽略资源的循环声明。
- [x] 测试动作完成后恢复 Idle。
- [ ] 测试模型切换后旧命令不会影响新模型。
- [x] 使用本地 Hiyori 验证 `Body -> Tap@Body -> Idle`。
- [ ] 使用真实桌面环境验证控件、透明区域、HiDPI 和窗口移动行为。

## 下一里程碑：表情状态系统

- [ ] 为表情控制器增加可用名称查询和当前状态。
- [ ] 区分基础表情与临时表情。
- [ ] 支持临时表情持续时间和到期恢复。
- [ ] 支持表情请求优先级和来源。
- [ ] 新请求替换旧请求后，旧请求不得清除新表情。
- [ ] 没有基础表情时支持淡出到无表情状态。
- [ ] 重复表情名称和无效参数产生诊断。
- [ ] 定义 `Neutral`、`Happy`、`Sad`、`Angry`、`Surprised`、`Thinking`、
  `Shy` 等受控语义表情。
- [ ] 按模型配置将语义表情映射到实际 expression 名称。
- [ ] 模型没有 expression 时安全降级为无操作。
- [ ] 准备包含 `.exp3.json` 的本地模型进行真实视觉验收。

## 模型能力与设置

- [ ] 扩展模型能力目录，记录 motion groups、expression、Physics、Pose、
  EyeBlink 和 LipSync。
- [ ] 区分基础模型致命错误与可选动作、表情的局部错误。
- [ ] 在设置面板展示 HitArea、动作组、表情和能力诊断。
- [ ] 提供动作预览命令。
- [ ] 提供表情预览命令。
- [ ] 提供 HitArea 包围盒调试显示和最近命中区域。
- [ ] 保存模型级 HitArea、动作和语义表情映射，不修改第三方模型文件。
- [x] 校验模型资源引用路径和符号链接边界，防止读取模型目录外文件。
- [x] 为纹理数量、单边尺寸、文件大小和总解码像素增加上限。

## 自主行为与更多 Live2D 能力

- [ ] 在没有交互打断时按时长策略主动轮换多个 Idle 动作。
- [ ] 支持 `Flick@{Name}` 等拖拽或甩动手势动作。
- [ ] 利用 `EyeBlink` 参数组实现自动眨眼，并处理与 motion 的优先关系。
- [ ] 在有音频振幅输入后利用 `LipSync` 参数组实现口型同步。
- [ ] 评估呼吸参数或模型专用呼吸动作，避免硬编码所有模型不存在的参数。
- [ ] 评估更精确的三角形 HitArea；只有包围盒误触明确时才增加复杂度。
- [ ] 评估 motion 淡入淡出和多动作混合。
- [ ] 评估 Mocari 对 Motion UserData、声音引用和事件回调的扩展空间。

## 聊天与桌宠反应

- [x] 在统一配置中管理多个语言模型、当前模型和系统提示词。
- [x] 使用显式 Provider 映射、endpoint 校验和用户直接填写的凭据构造 genai Client。
- [x] 实现可停止、可拒绝迟到结果并按批次刷新 UI 的流式文本对话。
- [x] 在桌宠窗口提供聊天开关、底部单行输入栏，以及可滚动并在终态后渐隐的主题回复层。
- [x] 有界保存单个会话，并在重启后把未完成回复恢复为中断状态。
- [x] 对 Provider 错误进行分类脱敏，不记录请求正文、响应正文或系统提示词。
- [x] 支持有界图片附件输入；会话只持久化安全元数据，不保存图片像素。
- [x] 提供默认关闭、需用户显式授权且执行前后复核 revision 的 Agent 截屏工具。
- [ ] 在 `genai` 支持传输层大小限制后，于 HTTP/SSE 解析分配前限制单帧与错误响应体；当前
  应用层上限只覆盖依赖已解析出的事件。
- [ ] 定义与 LLM Provider 无关的 `AvatarReaction` 领域事件。
- [ ] LLM 只能请求 allowlist 中的语义表情和动作提示。
- [ ] 按完整语义片段更新反应，禁止每个 token 触发一次模型变化。
- [ ] 会话取消、替换和窗口关闭时清理过期反应。
- [ ] 对话表情、点击表情和系统预览使用明确的优先级规则。
- [ ] 日志不得记录完整对话或未脱敏的用户内容。

## 数据与 Agent 记忆

- [x] 使用 SurrealKV 作为生产嵌入式数据库，并仅在测试中启用 Mem 后端。
- [x] 生产依赖只启用 SurrealKV 与 Rustls，不启用远程协议、原生 TLS、HTTP、脚本、ML
  或 SurrealDB allocator。
- [x] 通过 `database` façade 隐藏 SurrealDB 类型，并提供有界版本化文档存储。
- [x] 建立 `agent_memory` 的中期、长期记忆字段及标量、标签和全文检索索引。
- [x] 将 Agent 会话保存到固定的 `./data/lunamate.db`，不保留旧文件兼容路径。
- [x] 串行提交会话 revision，拒绝迟到快照覆盖当前进程中的新状态。
- [ ] 选定 embedding 模型和稳定维度后，为 `agent_memory.embedding` 创建向量索引。
- [ ] 定义记忆提取、合并、过期、召回和删除策略，并限制每个 Agent 的记录数与总字节数。
- [ ] 将检索结果转换为有界、可诊断且不包含凭据的 Provider 上下文。

## 平台输入与窗口行为

- [x] Windows/macOS 托盘右键优先显示主题化 GPUI 毛玻璃菜单，Linux、调试开关和
  创建失败时回退原生菜单。
- [ ] 在真实 Windows/macOS 与混合 DPI 多显示器上验证托盘菜单定位、圆角、毛玻璃和
  原生回退开关。
- [ ] 将系统级透明区域点击穿透作为独立平台功能设计。
- [ ] 分别验证 Windows、Wayland、X11 和 macOS 的输入区域能力。
- [ ] 点击穿透启用时仍保证设置、关闭和移动按钮可交互。
- [ ] 评估点击穿透与当前全窗口视线跟随之间的冲突。
- [ ] 在不同缩放显示器之间移动窗口时保持视觉与命中坐标一致。

## 持续验证

每次 Rust 代码变更后依次执行：

```bash
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all
cargo test --locked
```

涉及鼠标命中、透明窗口、缩放、窗口移动和置顶行为的变更还必须在真实桌面环境
手工验证。`models/` 中的第三方模型只用于本地测试，不纳入版本库或发布包。
