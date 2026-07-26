//! 将 Mocari 的 Drawable 数据转换为 GPUI 可显示的 BGRA 图像。
//!
//! 本模块持有可复用的 CPU 光栅缓冲与静态渲染计划，不负责推进模型动画状态。

pub(in crate::model) mod rasterizer;

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::RenderImage;
use image::{Frame, RgbaImage};
use mocari::moc3::Moc3DrawableMesh;
use mocari::{assets::DecodedTexture, core::draw_order_from_raw};

use self::rasterizer::{PixelBounds, Rasterizer};

const MODEL_PADDING: f32 = 0.04;
const MAX_RASTER_PIXELS: usize = 1_280 * 1_280;
const MAX_DRAWABLES: usize = 4_096;
const MAX_TOTAL_VERTICES: usize = 1_000_000;
const MAX_TOTAL_INDICES: usize = 3_000_000;
pub(in crate::model) const MAX_MASK_CONTEXTS: usize = 32;
pub(in crate::model) const CANCEL_CHECK_PIXELS: usize = 4_096;
const CANCEL_CHECK_VERTICES: usize = 256;

/// generation 共享的单向取消令牌。
#[derive(Clone, Default)]
pub(crate) struct RenderCancellation {
    cancelled: Arc<AtomicBool>,
}

impl RenderCancellation {
    /// 标记当前 generation 的加载或渲染工作已经失效。
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// 返回当前 generation 是否已经被替换或关闭。
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// 在可中断边界检查取消状态。
    pub(super) fn checkpoint(&self) -> Result<(), RenderError> {
        if self.is_cancelled() {
            Err(RenderError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// 保存一个模型 generation 可复用的 CPU 渲染资源。
pub(in crate::model) struct CpuRenderer {
    rasterizer: Rasterizer,
    texture_indices: Vec<usize>,
    drawable_masks: Vec<Option<usize>>,
    masks: Vec<MaskBuffer>,
    mask_pixel_count: usize,
    used_masks: Vec<bool>,
    pub(in crate::model) draw_order: Vec<usize>,
    seen_render_orders: Vec<bool>,
}

impl CpuRenderer {
    /// 验证模型复杂度并建立生命周期内不变的纹理与蒙版映射。
    pub(in crate::model) fn new(
        meshes: &[Moc3DrawableMesh],
        textures: &[DecodedTexture],
        width: u32,
        height: u32,
        cancellation: &RenderCancellation,
    ) -> Result<Self, RenderError> {
        cancellation.checkpoint()?;
        let pixel_count = checked_pixel_count(width, height)?;
        if meshes.len() > MAX_DRAWABLES {
            return Err(RenderError::new(format!(
                "Drawable 数量 {} 超过上限 {MAX_DRAWABLES}",
                meshes.len()
            )));
        }

        let mut total_vertices = 0_usize;
        let mut total_indices = 0_usize;
        let mut maximum_vertices = 0_usize;
        let mut texture_indices = Vec::with_capacity(meshes.len());
        let mut drawable_masks = Vec::with_capacity(meshes.len());
        let mut masks = Vec::<MaskBuffer>::new();

        for (drawable_index, drawable) in meshes.iter().enumerate() {
            cancellation.checkpoint()?;
            let vertices = drawable.vertices();
            let indices = drawable.indices();
            total_vertices = total_vertices
                .checked_add(vertices.len())
                .ok_or_else(|| RenderError::new("Drawable 顶点总数发生整数溢出"))?;
            total_indices = total_indices
                .checked_add(indices.len())
                .ok_or_else(|| RenderError::new("Drawable 索引总数发生整数溢出"))?;
            maximum_vertices = maximum_vertices.max(vertices.len());
            if total_vertices > MAX_TOTAL_VERTICES || total_indices > MAX_TOTAL_INDICES {
                return Err(RenderError::new(format!(
                    "模型网格复杂度超过上限：顶点 {total_vertices}/{MAX_TOTAL_VERTICES}，索引 {total_indices}/{MAX_TOTAL_INDICES}"
                )));
            }
            if indices.len() % 3 != 0 {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 的三角形索引数量不是 3 的倍数"
                )));
            }
            if indices
                .iter()
                .any(|index| usize::from(*index) >= vertices.len())
            {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 包含越界顶点索引"
                )));
            }
            if vertices
                .iter()
                .any(|vertex| vertex.uv().iter().any(|coordinate| !coordinate.is_finite()))
            {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 包含非有限纹理坐标"
                )));
            }

            let texture_index = usize::try_from(drawable.texture_index()).map_err(|_| {
                RenderError::new(format!("Drawable {drawable_index} 包含负纹理索引"))
            })?;
            if texture_index >= textures.len() {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 引用了不存在的纹理 {texture_index}"
                )));
            }
            texture_indices.push(texture_index);

            if drawable.masks().is_empty() {
                drawable_masks.push(None);
                continue;
            }

            let sources = sorted_mask_sources(drawable.masks())?;
            if sources.iter().any(|source| *source >= meshes.len()) {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 包含越界蒙版索引"
                )));
            }
            let mask_index = match masks.iter().position(|mask| mask.sources == sources) {
                Some(index) => index,
                None => {
                    if masks.len() >= MAX_MASK_CONTEXTS {
                        return Err(RenderError::new(format!(
                            "模型蒙版上下文数量超过上限 {MAX_MASK_CONTEXTS}"
                        )));
                    }
                    masks.push(MaskBuffer {
                        sources,
                        alpha: Vec::new(),
                        dirty: None,
                    });
                    masks.len() - 1
                }
            };
            drawable_masks.push(Some(mask_index));
        }

        cancellation.checkpoint()?;
        let mask_count = masks.len();
        Ok(Self {
            rasterizer: Rasterizer::new(width, height, pixel_count, maximum_vertices)?,
            texture_indices,
            drawable_masks,
            masks,
            mask_pixel_count: pixel_count,
            used_masks: vec![false; mask_count],
            draw_order: (0..meshes.len()).collect(),
            seen_render_orders: vec![false; meshes.len()],
        })
    }

    /// 光栅化当前 Drawable 状态；大缓冲在相邻帧之间复用。
    pub(in crate::model) fn render(
        &mut self,
        meshes: &[Moc3DrawableMesh],
        textures: &[DecodedTexture],
        transform: ModelTransform,
        cancellation: &RenderCancellation,
    ) -> Result<RenderImage, RenderError> {
        cancellation.checkpoint()?;
        if meshes.len() != self.texture_indices.len() {
            return Err(RenderError::new("模型 Drawable 数量在运行期间发生变化"));
        }

        self.rasterizer.begin_frame(cancellation)?;
        self.update_draw_order(meshes);
        self.used_masks.fill(false);
        for (drawable_index, drawable) in meshes.iter().enumerate() {
            cancellation.checkpoint()?;
            if drawable.opacity() > 0.0
                && !drawable.vertices().is_empty()
                && let Some(mask_index) = self.drawable_masks[drawable_index]
            {
                self.used_masks[mask_index] = true;
            }
        }

        // 蒙版来源与目标映射在加载时固定；每帧只重建当前可见目标实际使用的上下文。
        for (mask_index, mask) in self.masks.iter_mut().enumerate() {
            cancellation.checkpoint()?;
            if !self.used_masks[mask_index] {
                continue;
            }
            if mask.alpha.is_empty() {
                mask.alpha = filled_vec(self.mask_pixel_count, 0.0_f32, "蒙版缓冲")?;
                mask.dirty = None;
            } else {
                // 只清空上一帧写入过的区域，避免每帧对整块蒙版缓冲做 memset。
                self.rasterizer
                    .clear_mask_region(&mut mask.alpha, mask.dirty, cancellation)?;
            }
            let mut dirty = None;
            for &source_index in &mask.sources {
                cancellation.checkpoint()?;
                let drawable = &meshes[source_index];
                let texture = &textures[self.texture_indices[source_index]];
                if let Some(bounds) = self.rasterizer.draw_mask(
                    drawable,
                    source_index,
                    texture,
                    transform,
                    &mut mask.alpha,
                    cancellation,
                )? {
                    dirty = Some(match dirty {
                        Some(existing) => bounds.union(existing),
                        None => bounds,
                    });
                }
            }
            mask.dirty = dirty;
        }

        for &drawable_index in &self.draw_order {
            cancellation.checkpoint()?;
            let drawable = &meshes[drawable_index];
            let opacity = drawable.opacity();
            if !opacity.is_finite() {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 的透明度不是有限值"
                )));
            }
            if opacity <= 0.0 || drawable.vertices().is_empty() {
                continue;
            }

            let texture = &textures[self.texture_indices[drawable_index]];
            let mask = self.drawable_masks[drawable_index].map(|mask_index| {
                (
                    self.masks[mask_index].alpha.as_slice(),
                    drawable.is_inverted_mask(),
                )
            });
            self.rasterizer.draw(
                drawable,
                drawable_index,
                texture,
                transform,
                mask,
                cancellation,
            )?;
        }

        let bgra = self.rasterizer.straight_bgra8(cancellation)?;
        self.rasterizer.end_frame();
        let [width, height] = self.rasterizer.dimensions();
        let image = RgbaImage::from_raw(width, height, bgra)
            .ok_or_else(|| RenderError::new("无法构造 GPUI 图像缓冲"))?;
        Ok(RenderImage::new(vec![Frame::new(image)]))
    }

    pub(in crate::model) fn update_draw_order(&mut self, meshes: &[Moc3DrawableMesh]) {
        self.seen_render_orders.fill(false);
        let mut total_rank = !meshes.is_empty();
        let mut identity_rank = true;
        for (drawable_index, drawable) in meshes.iter().enumerate() {
            let Ok(rank) = usize::try_from(drawable.render_order()) else {
                total_rank = false;
                break;
            };
            let Some(seen) = self.seen_render_orders.get_mut(rank) else {
                total_rank = false;
                break;
            };
            if *seen {
                total_rank = false;
                break;
            }
            *seen = true;
            self.draw_order[rank] = drawable_index;
            identity_rank &= rank == drawable_index;
        }
        if total_rank && !identity_rank {
            return;
        }

        for (index, slot) in self.draw_order.iter_mut().enumerate() {
            *slot = index;
        }
        self.draw_order.sort_unstable_by(|left, right| {
            draw_order_from_raw(meshes[*left].draw_order())
                .cmp(&draw_order_from_raw(meshes[*right].draw_order()))
                .then_with(|| {
                    meshes[*left]
                        .render_order()
                        .cmp(&meshes[*right].render_order())
                })
                .then_with(|| left.cmp(right))
        });
    }
}

struct MaskBuffer {
    sources: Vec<usize>,
    alpha: Vec<f32>,
    /// 上一帧写入过的区域；下一帧只需清空这部分。
    dirty: Option<PixelBounds>,
}

pub(in crate::model) fn sorted_mask_sources(indices: &[i32]) -> Result<Vec<usize>, RenderError> {
    let mut sources = Vec::with_capacity(indices.len());
    for &index in indices {
        sources.push(
            usize::try_from(index).map_err(|_| RenderError::new("模型包含负蒙版 Drawable 索引"))?,
        );
    }
    sources.sort_unstable();
    Ok(sources)
}

/// 验证 GPU 路径共享的静态网格上限，但不分配 CPU 光栅缓冲。
pub(in crate::model) fn validate_gpu_model(
    meshes: &[Moc3DrawableMesh],
    textures: &[DecodedTexture],
    width: u32,
    height: u32,
    cancellation: &RenderCancellation,
) -> Result<(), RenderError> {
    cancellation.checkpoint()?;
    if width == 0 || height == 0 {
        return Err(RenderError::new("GPU surface 尺寸必须非零"));
    }
    if meshes.len() > MAX_DRAWABLES {
        return Err(RenderError::new(format!(
            "Drawable 数量 {} 超过上限 {MAX_DRAWABLES}",
            meshes.len()
        )));
    }

    let mut total_vertices = 0_usize;
    let mut total_indices = 0_usize;
    let mut mask_contexts = Vec::<(Vec<usize>, bool)>::new();
    for (drawable_index, drawable) in meshes.iter().enumerate() {
        cancellation.checkpoint()?;
        let vertices = drawable.vertices();
        let indices = drawable.indices();
        total_vertices = total_vertices
            .checked_add(vertices.len())
            .ok_or_else(|| RenderError::new("Drawable 顶点总数发生整数溢出"))?;
        total_indices = total_indices
            .checked_add(indices.len())
            .ok_or_else(|| RenderError::new("Drawable 索引总数发生整数溢出"))?;
        if total_vertices > MAX_TOTAL_VERTICES || total_indices > MAX_TOTAL_INDICES {
            return Err(RenderError::new(format!(
                "模型网格复杂度超过上限：顶点 {total_vertices}/{MAX_TOTAL_VERTICES}，索引 {total_indices}/{MAX_TOTAL_INDICES}"
            )));
        }
        if indices.len() % 3 != 0
            || indices
                .iter()
                .any(|index| usize::from(*index) >= vertices.len())
        {
            return Err(RenderError::new(format!(
                "Drawable {drawable_index} 包含无效三角形索引"
            )));
        }
        if vertices
            .iter()
            .any(|vertex| vertex.uv().iter().any(|coordinate| !coordinate.is_finite()))
        {
            return Err(RenderError::new(format!(
                "Drawable {drawable_index} 包含非有限纹理坐标"
            )));
        }
        let texture_index = usize::try_from(drawable.texture_index())
            .map_err(|_| RenderError::new(format!("Drawable {drawable_index} 包含负纹理索引")))?;
        if texture_index >= textures.len() {
            return Err(RenderError::new(format!(
                "Drawable {drawable_index} 引用了不存在的纹理 {texture_index}"
            )));
        }
        if !drawable.masks().is_empty() {
            let sources = sorted_mask_sources(drawable.masks())?;
            if sources.iter().any(|source| *source >= meshes.len()) {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 包含越界蒙版索引"
                )));
            }
            let context = (sources, drawable.is_inverted_mask());
            if !mask_contexts.contains(&context) {
                mask_contexts.push(context);
                if mask_contexts.len() > MAX_MASK_CONTEXTS {
                    return Err(RenderError::new(format!(
                        "模型蒙版上下文数量超过上限 {MAX_MASK_CONTEXTS}"
                    )));
                }
            }
        }
    }
    validate_gpu_frame(meshes, cancellation)
}

/// 拒绝非有限动态值；有限越界值由 CPU 与 GPU 共同按 0..=1 截断。
pub(in crate::model) fn validate_gpu_frame(
    meshes: &[Moc3DrawableMesh],
    cancellation: &RenderCancellation,
) -> Result<(), RenderError> {
    for (drawable_index, drawable) in meshes.iter().enumerate() {
        cancellation.checkpoint()?;
        let opacity = drawable.opacity();
        if !opacity.is_finite()
            || drawable
                .multiply_color()
                .iter()
                .chain(drawable.screen_color().iter())
                .any(|channel| !channel.is_finite())
            || drawable
                .vertices()
                .iter()
                .any(|vertex| vertex.position().iter().any(|value| !value.is_finite()))
        {
            return Err(RenderError::new(format!(
                "Drawable {drawable_index} 包含非有限 GPU 绘制数据"
            )));
        }
    }
    Ok(())
}

/// 描述模型坐标到离屏光栅坐标的固定变换。
#[derive(Clone, Copy)]
pub(in crate::model) struct ModelTransform {
    scale: f32,
    center_x: f32,
    center_y: f32,
    image_center_x: f32,
    image_center_y: f32,
}

impl ModelTransform {
    /// 根据初始 Drawable 包围盒建立保持宽高比的居中变换。
    pub(in crate::model) fn fit(
        meshes: &[Moc3DrawableMesh],
        width: u32,
        height: u32,
        cancellation: &RenderCancellation,
    ) -> Result<Self, RenderError> {
        cancellation.checkpoint()?;
        if width == 0 || height == 0 {
            return Err(RenderError::new("模型变换目标尺寸必须非零"));
        }
        let mut bounds: Option<[f32; 4]> = None;
        for mesh in meshes {
            cancellation.checkpoint()?;
            for (vertex_index, vertex) in mesh.vertices().iter().enumerate() {
                if vertex_index % CANCEL_CHECK_VERTICES == 0 {
                    cancellation.checkpoint()?;
                }
                let [x, y] = vertex.position();
                if !x.is_finite() || !y.is_finite() {
                    return Err(RenderError::new("模型初始 Drawable 包含非有限顶点坐标"));
                }
                bounds = Some(match bounds {
                    Some([min_x, min_y, max_x, max_y]) => {
                        [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
                    }
                    None => [x, y, x, y],
                });
            }
        }

        let [min_x, min_y, max_x, max_y] =
            bounds.ok_or_else(|| RenderError::new("模型没有 Drawable 顶点"))?;
        let model_width = max_x - min_x;
        let model_height = max_y - min_y;
        if !model_width.is_finite()
            || !model_height.is_finite()
            || model_width <= 0.0
            || model_height <= 0.0
        {
            return Err(RenderError::new("模型 Drawable 包围盒无效"));
        }

        let usable_width = width as f32 * (1.0 - MODEL_PADDING * 2.0);
        let usable_height = height as f32 * (1.0 - MODEL_PADDING * 2.0);
        let scale = (usable_width / model_width).min(usable_height / model_height);
        if !scale.is_finite() || scale <= 0.0 {
            return Err(RenderError::new("模型到光栅坐标的缩放比例无效"));
        }

        Ok(Self {
            scale,
            center_x: (min_x + max_x) * 0.5,
            center_y: (min_y + max_y) * 0.5,
            image_center_x: width as f32 * 0.5,
            image_center_y: height as f32 * 0.5,
        })
    }

    /// 将一个模型坐标点转换为离屏光栅坐标。
    pub(super) fn point(self, position: [f32; 2]) -> [f32; 2] {
        [
            (position[0] - self.center_x) * self.scale + self.image_center_x,
            (self.center_y - position[1]) * self.scale + self.image_center_y,
        ]
    }

    /// 返回把同一模型坐标映射到 WGPU clip space 的矩阵。
    pub(super) fn clip_matrix(self, width: u32, height: u32) -> mocari::core::Matrix44 {
        let scale_x = 2.0 * self.scale / width as f32;
        let scale_y = 2.0 * self.scale / height as f32;
        let mut matrix = mocari::core::Matrix44::identity();
        matrix.scale(scale_x, scale_y);
        matrix.translate(-self.center_x * scale_x, -self.center_y * scale_y);
        matrix
    }
}

pub(in crate::model) fn checked_pixel_count(width: u32, height: u32) -> Result<usize, RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::new("渲染尺寸必须非零"));
    }
    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| RenderError::new("渲染像素数量发生整数溢出"))?;
    if pixel_count > MAX_RASTER_PIXELS {
        return Err(RenderError::new(format!(
            "渲染像素数量 {pixel_count} 超过上限 {MAX_RASTER_PIXELS}"
        )));
    }
    Ok(pixel_count)
}

fn filled_vec<T: Clone>(length: usize, value: T, label: &str) -> Result<Vec<T>, RenderError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|error| RenderError::new(format!("无法分配{label}：{error}")))?;
    values.resize(length, value);
    Ok(values)
}

pub(in crate::model) fn fill_cancelable<T: Clone>(
    values: &mut [T],
    value: T,
    cancellation: &RenderCancellation,
) -> Result<(), RenderError> {
    for chunk in values.chunks_mut(CANCEL_CHECK_PIXELS) {
        cancellation.checkpoint()?;
        chunk.fill(value.clone());
    }
    Ok(())
}

/// 描述 CPU 渲染计划或单帧光栅化失败。
#[derive(Debug)]
pub(crate) enum RenderError {
    /// 当前 generation 已被模型切换或窗口关闭取消。
    Cancelled,
    /// 模型数据或渲染状态无法生成有效图像。
    Failed { message: String },
}

impl RenderError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
        }
    }

    /// 返回错误是否只表示当前 generation 已失效。
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("渲染已取消"),
            Self::Failed { message } => formatter.write_str(message),
        }
    }
}

impl Error for RenderError {}
