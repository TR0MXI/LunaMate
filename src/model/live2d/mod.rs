//! 负责 Live2D 动画运行时更新，并为 CPU 回退和原生 GPU underlay 提供帧数据。

pub(in crate::model) mod gpu_renderer;
pub(in crate::model) mod renderer;
pub(in crate::model) mod resources;

use std::{error::Error, fmt, path::Path, time::Duration};

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use gpui::RenderImage;
use mocari::assets::{AssetLoadError, RuntimeModel, load_model_runtime};
use rust_i18n::t;

use super::{
    animation::{self, AnimationController, MotionPlayResult},
    capabilities::{AuxiliaryResourceBudget, ModelCapabilities, ModelLoadDiagnostics},
    expression::{self, ExpressionController},
    interaction::{ModelCommand, RenderedModelFrame, render_hit_areas},
};

pub(crate) use self::gpu_renderer::{GpuModelRenderer, SurfaceAlphaMode};
use self::renderer::{CpuRenderer, ModelTransform};
pub(crate) use self::renderer::{RenderCancellation, RenderError};
use self::resources::{ResourceValidationError, validate_model_resources};

/// 设置窗口可以向用户展示并触发的已加载模型能力。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelPreviewCapabilities {
    outfits: Vec<String>,
    motions: Vec<String>,
    expressions: Vec<String>,
}

impl ModelPreviewCapabilities {
    /// 构造不依赖真实模型的能力快照，供设置界面状态测试使用。
    #[cfg(test)]
    pub(crate) fn new_for_test(
        outfits: Vec<String>,
        motions: Vec<String>,
        expressions: Vec<String>,
    ) -> Self {
        Self {
            outfits,
            motions,
            expressions,
        }
    }

    /// 返回从模型目录外部表达式发现的服装名称。
    pub(crate) fn outfits(&self) -> &[String] {
        &self.outfits
    }

    /// 返回至少包含一个有效动作的动作组。
    pub(crate) fn motions(&self) -> &[String] {
        &self.motions
    }

    /// 返回全部有效表情名称。
    pub(crate) fn expressions(&self) -> &[String] {
        &self.expressions
    }
}

/// 描述阻止模型主体进入可渲染状态的致命加载错误。
#[derive(Debug)]
pub(crate) enum ModelLoadError {
    /// 当前 generation 在加载完成前已被替换或关闭。
    Cancelled,
    /// 请求的离屏光栅尺寸至少有一边为零。
    InvalidRasterDimensions { width: u32, height: u32 },
    /// 清单、MOC、纹理、Physics 或 Pose 未通过必需资源预检。
    RequiredResources(ResourceValidationError),
    /// Mocari 无法解析主体运行时或解码纹理。
    Runtime(AssetLoadError),
    /// 无法根据主体 Drawable 建立模型到光栅的坐标变换。
    Transform(RenderError),
    /// 无法根据主体 Drawable 和纹理建立 CPU 渲染计划。
    Renderer(RenderError),
}

impl fmt::Display for ModelLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("模型加载已取消"),
            Self::InvalidRasterDimensions { width, height } => {
                write!(formatter, "光栅尺寸必须非零，当前为 {width}x{height}")
            }
            Self::RequiredResources(error) => write!(formatter, "必需资源预检失败：{error}"),
            Self::Runtime(error) => write!(formatter, "Mocari 主体加载失败：{error}"),
            Self::Transform(error) => write!(formatter, "模型坐标变换初始化失败：{error}"),
            Self::Renderer(error) => write!(formatter, "CPU 渲染器初始化失败：{error}"),
        }
    }
}

impl Error for ModelLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cancelled | Self::InvalidRasterDimensions { .. } => None,
            Self::RequiredResources(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Transform(error) | Self::Renderer(error) => Some(error),
        }
    }
}

impl ModelLoadError {
    /// 返回错误是否只表示当前 generation 已失效。
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

#[cfg(test)]
pub(in crate::model) fn render_model(
    path: &Path,
    size: u32,
) -> Result<Arc<RenderImage>, Box<dyn Error>> {
    let mut model = AnimatedModel::load(path, size, size, RenderCancellation::default())?;
    let frame = model.render_frame(Duration::ZERO, [0.0, 0.0])?;
    frame
        .image()
        .cloned()
        .ok_or_else(|| "CPU 渲染帧必须包含 GPUI 图像".into())
}

/// 持有一个模型 generation 的可变 Live2D 状态、动作和表情控制器。
pub(crate) struct AnimatedModel {
    model: RuntimeModel,
    capabilities: ModelCapabilities,
    animation: AnimationController,
    expression: ExpressionController,
    diagnostics: ModelLoadDiagnostics,
    transform: ModelTransform,
    renderer: Option<CpuRenderer>,
    width: u32,
    height: u32,
    cancellation: RenderCancellation,
}

impl AnimatedModel {
    /// 加载模型及其动作、表情和可交互能力。
    ///
    /// # Errors
    ///
    /// 模型主体、必需资源或初始 Drawable 数据无法读取时返回错误。坏动作、表情和 HitArea
    /// 只会进入 [`ModelLoadDiagnostics`]，不会阻止模型主体加载。
    pub(crate) fn load(
        path: &Path,
        width: u32,
        height: u32,
        cancellation: RenderCancellation,
    ) -> Result<Self, ModelLoadError> {
        Self::load_with_cpu_renderer(path, width, height, cancellation, true)
    }

    /// 加载由原生 GPU underlay 呈现的模型，不分配 CPU 光栅缓冲。
    ///
    /// # Errors
    ///
    /// 模型主体、必需资源或初始 Drawable 数据无法读取时返回错误。
    pub(crate) fn load_for_gpu(
        path: &Path,
        width: u32,
        height: u32,
        cancellation: RenderCancellation,
    ) -> Result<Self, ModelLoadError> {
        Self::load_with_cpu_renderer(path, width, height, cancellation, false)
    }

    fn load_with_cpu_renderer(
        path: &Path,
        width: u32,
        height: u32,
        cancellation: RenderCancellation,
        create_cpu_renderer: bool,
    ) -> Result<Self, ModelLoadError> {
        if width == 0 || height == 0 {
            return Err(ModelLoadError::InvalidRasterDimensions { width, height });
        }

        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        let resolver = validate_model_resources(path).map_err(ModelLoadError::RequiredResources)?;
        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        // Mocari 当前仍会按路径重新打开清单和必需资源；预检只能限制稳定文件，无法消除
        // 两次打开之间的替换窗口。彻底关闭该 TOCTOU 需要加载器接收已验证资源句柄。
        let model = load_model_runtime(path).map_err(ModelLoadError::Runtime)?;
        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        let transform = ModelTransform::fit(model.runtime().meshes(), width, height, &cancellation)
            .map_err(|error| {
                if error.is_cancelled() {
                    ModelLoadError::Cancelled
                } else {
                    ModelLoadError::Transform(error)
                }
            })?;
        let renderer = create_cpu_renderer
            .then(|| {
                CpuRenderer::new(
                    model.runtime().meshes(),
                    model.textures(),
                    width,
                    height,
                    &cancellation,
                )
            })
            .transpose()
            .map_err(|error| {
                if error.is_cancelled() {
                    ModelLoadError::Cancelled
                } else {
                    ModelLoadError::Renderer(error)
                }
            })?;
        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        let (capabilities, mut diagnostics) = ModelCapabilities::inspect(&model);
        let mut auxiliary_budget = AuxiliaryResourceBudget::default();
        let (animation, animation_diagnostics) =
            animation::load(&model, &resolver, &mut auxiliary_budget, &cancellation);
        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        let (expression, expression_diagnostics) =
            expression::load(&model, &resolver, &mut auxiliary_budget, &cancellation);
        cancellation
            .checkpoint()
            .map_err(|_| ModelLoadError::Cancelled)?;
        diagnostics.extend(animation_diagnostics);
        diagnostics.extend(expression_diagnostics);
        for diagnostic in diagnostics.entries() {
            log::warn!(
                "{}",
                t!("log.model_capability_warning", diagnostic = diagnostic)
            );
        }

        Ok(Self {
            model,
            capabilities,
            animation,
            expression,
            diagnostics,
            transform,
            renderer,
            width,
            height,
            cancellation,
        })
    }

    /// 返回不会阻止模型主体运行的动作、表情和 HitArea 诊断。
    pub(crate) fn diagnostics(&self) -> &ModelLoadDiagnostics {
        &self.diagnostics
    }

    /// 返回设置窗口可实际触发的动作与表情名称。
    pub(crate) fn preview_capabilities(&self) -> ModelPreviewCapabilities {
        ModelPreviewCapabilities {
            outfits: self.expression.available_outfits(),
            motions: self.animation.available_groups(),
            expressions: self.expression.available_names(),
        }
    }

    /// 返回模型是否仍包含必须按时间推进的状态。
    pub(crate) fn needs_continuous_frames(&self) -> bool {
        self.animation.needs_continuous_frames()
            || self.expression.needs_continuous_frames()
            || self.model.runtime().physics().is_some()
            || self.model.runtime().model().pose().is_some()
    }

    /// 处理一个离散模型命令，并返回命令是否启动了可见交互。
    pub(crate) fn handle_command(&mut self, command: ModelCommand) -> bool {
        match command {
            ModelCommand::PreviewMotion(group) => matches!(
                self.animation.play_interaction(&group),
                MotionPlayResult::Started
            ),
            ModelCommand::PreviewExpression(name) => self.expression.play(&name),
            ModelCommand::ResetExpression => self.expression.reset_to_default(),
        }
    }

    /// 推进模型状态，并生成图像及同一状态下的 HitArea 快照。
    ///
    /// # Errors
    ///
    /// Drawable 更新或 CPU 光栅化失败时返回错误。
    pub(crate) fn render_frame(
        &mut self,
        delta: Duration,
        look: [f32; 2],
    ) -> Result<RenderedModelFrame, RenderError> {
        let hit_areas = self.advance_frame(delta, look)?;
        let renderer = self
            .renderer
            .as_mut()
            .ok_or_else(|| RenderError::new("GPU 模型不能进入 CPU 光栅路径"))?;
        let image = renderer.render(
            self.model.runtime().meshes(),
            self.model.textures(),
            self.transform,
            &self.cancellation,
        )?;
        Ok(RenderedModelFrame::new(
            image,
            hit_areas,
            [self.width, self.height],
        ))
    }

    /// 推进运行时并返回与更新后 Drawable 对应的 HitArea。
    fn advance_frame(
        &mut self,
        delta: Duration,
        look: [f32; 2],
    ) -> Result<Vec<super::interaction::RenderedHitArea>, RenderError> {
        self.cancellation.checkpoint()?;
        let delta = delta.min(Duration::from_millis(100));
        let delta_seconds = delta.as_secs_f32();
        // 每帧从模型默认值重新合成动作、表情和交互参数，避免上一帧覆盖值残留。
        {
            let runtime = self.model.runtime_mut();
            runtime.reset_parameters();
            runtime.reset_part_opacities();
            self.animation.update(runtime, delta_seconds);
            self.expression.update(runtime, delta_seconds);

            offset_parameter(runtime, "ParamAngleX", look[0] * 15.0);
            offset_parameter(runtime, "ParamAngleY", look[1] * 10.0);
            offset_parameter(runtime, "ParamBodyAngleX", look[0] * 5.0);
            runtime.set_parameter("ParamEyeBallX", look[0]);
            runtime.set_parameter("ParamEyeBallY", look[1]);
            runtime.apply_physics(delta_seconds);
            runtime.apply_pose(delta_seconds);
            runtime
                .update_meshes()
                .ok_or_else(|| RenderError::new("无法更新 Live2D Drawable"))?;
        }
        self.cancellation.checkpoint()?;

        // HitArea 必须从刚更新的 Drawable 生成，确保点击检测与用户看到的画面一致。
        Ok(render_hit_areas(
            self.capabilities.hit_areas(),
            self.capabilities.hit_area_bounds_count(),
            self.model.runtime().meshes(),
            |position| self.transform.point(position),
        ))
    }

    fn runtime_model(&self) -> &RuntimeModel {
        &self.model
    }

    fn transform(&self) -> ModelTransform {
        self.transform
    }

    fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    fn cancellation(&self) -> &RenderCancellation {
        &self.cancellation
    }
}

fn offset_parameter(runtime: &mut mocari::ModelRuntime, id: &str, offset: f32) {
    if let Some(value) = runtime.parameter_value(id) {
        runtime.set_parameter(id, value + offset);
    }
}
