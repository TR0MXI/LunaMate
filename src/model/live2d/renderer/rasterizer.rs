//! 实现可取消的 CPU 三角形光栅化与像素合成。
//!
//! 本模块只处理已经验证的 Drawable 网格、纹理采样和离屏缓冲，不负责模型复杂度校验、
//! Drawable 排序或蒙版上下文调度。

use mocari::{
    assets::DecodedTexture,
    moc3::{Moc3DrawableBlendMode, Moc3DrawableMesh},
};

use super::{
    CANCEL_CHECK_PIXELS, CANCEL_CHECK_VERTICES, ModelTransform, RenderCancellation, RenderError,
    fill_cancelable, filled_vec,
};

const CANCEL_CHECK_TRIANGLES: usize = 64;

/// 持有跨帧复用的颜色、临时图层和顶点变换缓冲。
pub(super) struct Rasterizer {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
    layer: Vec<[f32; 4]>,
    stamps: Vec<u32>,
    transformed_vertices: Vec<[f32; 2]>,
    current_stamp: u32,
}

impl Rasterizer {
    /// 分配固定尺寸的光栅缓冲，并为最大单个 Drawable 预留顶点空间。
    pub(super) fn new(
        width: u32,
        height: u32,
        pixel_count: usize,
        maximum_vertices: usize,
    ) -> Result<Self, RenderError> {
        let mut transformed_vertices = Vec::new();
        transformed_vertices
            .try_reserve_exact(maximum_vertices)
            .map_err(|error| RenderError::new(format!("无法分配顶点变换缓冲：{error}")))?;
        Ok(Self {
            width,
            height,
            pixels: filled_vec(pixel_count, [0.0; 4], "颜色缓冲")?,
            layer: filled_vec(pixel_count, [0.0; 4], "Drawable 临时缓冲")?,
            stamps: filled_vec(pixel_count, 0_u32, "Drawable 标记缓冲")?,
            transformed_vertices,
            current_stamp: 0,
        })
    }

    /// 返回离屏光栅缓冲的固定像素尺寸。
    pub(super) fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// 清空上一帧颜色，并在大缓冲处理期间响应取消。
    pub(super) fn begin_frame(
        &mut self,
        cancellation: &RenderCancellation,
    ) -> Result<(), RenderError> {
        fill_cancelable(&mut self.pixels, [0.0; 4], cancellation)
    }

    /// 光栅化并混合一个可见 Drawable，可选应用预先生成的蒙版。
    pub(super) fn draw(
        &mut self,
        drawable: &Moc3DrawableMesh,
        drawable_index: usize,
        texture: &DecodedTexture,
        transform: ModelTransform,
        mask: Option<(&[f32], bool)>,
        cancellation: &RenderCancellation,
    ) -> Result<(), RenderError> {
        let opacity = drawable.opacity().clamp(0.0, 1.0);
        let multiply = drawable.multiply_color();
        let screen = drawable.screen_color();
        if multiply
            .iter()
            .chain(screen.iter())
            .any(|channel| !channel.is_finite())
        {
            return Err(RenderError::new(format!(
                "Drawable {drawable_index} 包含非有限颜色值"
            )));
        }
        let multiply = multiply.map(|channel| channel.clamp(0.0, 1.0));
        let screen = screen.map(|channel| channel.clamp(0.0, 1.0));
        let blend_mode = drawable.blend_mode();
        let bounds = self.rasterize_layer(
            drawable,
            drawable_index,
            texture,
            transform,
            |sample| {
                let mut rgb = [0.0; 3];
                for channel in 0..3 {
                    let multiplied = sample[channel] * multiply[channel];
                    rgb[channel] = multiplied + screen[channel] - multiplied * screen[channel];
                }
                let alpha = sample[3] * opacity;
                [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha]
            },
            cancellation,
        )?;

        let Some(bounds) = bounds else {
            return Ok(());
        };
        for y in bounds.min_y..=bounds.max_y {
            cancellation.checkpoint()?;
            for x in bounds.min_x..=bounds.max_x {
                let index = self.index(x, y);
                if self.stamps[index] != self.current_stamp {
                    continue;
                }

                let mut source = self.layer[index];
                if let Some((mask_alpha, inverted)) = mask {
                    let mask_value = if inverted {
                        1.0 - mask_alpha[index]
                    } else {
                        mask_alpha[index]
                    }
                    .clamp(0.0, 1.0);
                    for channel in &mut source {
                        *channel *= mask_value;
                    }
                }
                blend(&mut self.pixels[index], source, blend_mode);
            }
        }
        Ok(())
    }

    /// 将一个 Drawable 的纹理 Alpha 累积到指定蒙版缓冲。
    pub(super) fn draw_mask(
        &mut self,
        drawable: &Moc3DrawableMesh,
        drawable_index: usize,
        texture: &DecodedTexture,
        transform: ModelTransform,
        alpha: &mut [f32],
        cancellation: &RenderCancellation,
    ) -> Result<(), RenderError> {
        let bounds = self.rasterize_layer(
            drawable,
            drawable_index,
            texture,
            transform,
            |sample| [0.0, 0.0, 0.0, sample[3]],
            cancellation,
        )?;

        let Some(bounds) = bounds else {
            return Ok(());
        };
        for y in bounds.min_y..=bounds.max_y {
            cancellation.checkpoint()?;
            for x in bounds.min_x..=bounds.max_x {
                let index = self.index(x, y);
                if self.stamps[index] == self.current_stamp {
                    alpha[index] = (alpha[index] + self.layer[index][3]).min(1.0);
                }
            }
        }
        Ok(())
    }

    fn rasterize_layer(
        &mut self,
        drawable: &Moc3DrawableMesh,
        drawable_index: usize,
        texture: &DecodedTexture,
        transform: ModelTransform,
        shade: impl Fn([f32; 4]) -> [f32; 4],
        cancellation: &RenderCancellation,
    ) -> Result<Option<PixelBounds>, RenderError> {
        self.next_stamp(cancellation)?;
        self.transformed_vertices.clear();
        for (vertex_index, vertex) in drawable.vertices().iter().enumerate() {
            if vertex_index % CANCEL_CHECK_VERTICES == 0 {
                cancellation.checkpoint()?;
            }
            let point = transform.point(vertex.position());
            if point.iter().any(|coordinate| !coordinate.is_finite()) {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 包含无法转换的非有限顶点"
                )));
            }
            self.transformed_vertices.push(point);
        }

        let vertices = drawable.vertices();
        let mut drawable_bounds: Option<PixelBounds> = None;
        for (triangle_index, triangle) in drawable.indices().chunks_exact(3).enumerate() {
            if triangle_index % CANCEL_CHECK_TRIANGLES == 0 {
                cancellation.checkpoint()?;
            }
            let index_0 = usize::from(triangle[0]);
            let index_1 = usize::from(triangle[1]);
            let index_2 = usize::from(triangle[2]);
            let points = [
                self.transformed_vertices[index_0],
                self.transformed_vertices[index_1],
                self.transformed_vertices[index_2],
            ];
            let area = edge(points[0], points[1], points[2]);
            if !area.is_finite() {
                return Err(RenderError::new(format!(
                    "Drawable {drawable_index} 的三角形面积不是有限值"
                )));
            }
            if area.abs() <= f32::EPSILON {
                continue;
            }

            let Some(bounds) = PixelBounds::from_triangle(points, self.width, self.height) else {
                continue;
            };
            drawable_bounds = Some(match drawable_bounds {
                Some(existing) => existing.union(bounds),
                None => bounds,
            });

            let uv_0 = vertices[index_0].uv();
            let uv_1 = vertices[index_1].uv();
            let uv_2 = vertices[index_2].uv();
            for y in bounds.min_y..=bounds.max_y {
                cancellation.checkpoint()?;
                for x in bounds.min_x..=bounds.max_x {
                    let point = [x as f32 + 0.5, y as f32 + 0.5];
                    let weights = [
                        edge(points[1], points[2], point) / area,
                        edge(points[2], points[0], point) / area,
                        edge(points[0], points[1], point) / area,
                    ];
                    if weights[0] < -0.000_01 || weights[1] < -0.000_01 || weights[2] < -0.000_01 {
                        continue;
                    }

                    let uv = [
                        uv_0[0] * weights[0] + uv_1[0] * weights[1] + uv_2[0] * weights[2],
                        uv_0[1] * weights[0] + uv_1[1] * weights[1] + uv_2[1] * weights[2],
                    ];
                    let index = self.index(x, y);
                    self.layer[index] = shade(sample_texture(texture, uv));
                    self.stamps[index] = self.current_stamp;
                }
            }
        }

        Ok(drawable_bounds)
    }

    fn next_stamp(&mut self, cancellation: &RenderCancellation) -> Result<(), RenderError> {
        self.current_stamp = self.current_stamp.wrapping_add(1);
        if self.current_stamp == 0 {
            fill_cancelable(&mut self.stamps, 0, cancellation)?;
            self.current_stamp = 1;
        }
        Ok(())
    }

    fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// 将内部预乘 RGBA 浮点颜色转换为 GPUI 使用的直通 BGRA8。
    pub(super) fn straight_bgra8(
        &self,
        cancellation: &RenderCancellation,
    ) -> Result<Vec<u8>, RenderError> {
        let byte_count = self
            .pixels
            .len()
            .checked_mul(4)
            .ok_or_else(|| RenderError::new("输出图像字节数发生整数溢出"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|error| RenderError::new(format!("无法分配输出图像缓冲：{error}")))?;
        for pixels in self.pixels.chunks(CANCEL_CHECK_PIXELS) {
            cancellation.checkpoint()?;
            for [red, green, blue, alpha] in pixels.iter().copied() {
                let alpha = alpha.clamp(0.0, 1.0);
                let unpremultiply = if alpha > 0.0 { 1.0 / alpha } else { 0.0 };
                bytes.extend_from_slice(&[
                    to_u8(blue * unpremultiply),
                    to_u8(green * unpremultiply),
                    to_u8(red * unpremultiply),
                    to_u8(alpha),
                ]);
            }
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy)]
struct PixelBounds {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

impl PixelBounds {
    fn from_triangle(points: [[f32; 2]; 3], width: u32, height: u32) -> Option<Self> {
        let min_x = points
            .iter()
            .map(|point| point[0])
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let min_y = points
            .iter()
            .map(|point| point[1])
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let max_x = points
            .iter()
            .map(|point| point[0])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .min(width.saturating_sub(1) as f32) as u32;
        let max_y = points
            .iter()
            .map(|point| point[1])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .min(height.saturating_sub(1) as f32) as u32;

        (min_x <= max_x && min_y <= max_y).then_some(Self {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }
}

pub(in crate::model) fn sample_texture(texture: &DecodedTexture, uv: [f32; 2]) -> [f32; 4] {
    let width = texture.width();
    let height = texture.height();
    if width == 0 || height == 0 {
        return [0.0; 4];
    }

    let x = uv[0].clamp(0.0, 1.0) * width.saturating_sub(1) as f32;
    let y = uv[1].clamp(0.0, 1.0) * height.saturating_sub(1) as f32;
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let top = mix(
        texture_pixel(texture, x0, y0),
        texture_pixel(texture, x1, y0),
        tx,
    );
    let bottom = mix(
        texture_pixel(texture, x0, y1),
        texture_pixel(texture, x1, y1),
        tx,
    );
    mix(top, bottom, ty)
}

fn texture_pixel(texture: &DecodedTexture, x: u32, y: u32) -> [f32; 4] {
    let offset = (y as usize * texture.width() as usize + x as usize) * 4;
    let rgba = &texture.rgba()[offset..offset + 4];
    [
        rgba[0] as f32 / 255.0,
        rgba[1] as f32 / 255.0,
        rgba[2] as f32 / 255.0,
        rgba[3] as f32 / 255.0,
    ]
}

fn mix(left: [f32; 4], right: [f32; 4], amount: f32) -> [f32; 4] {
    let mut mixed = [0.0; 4];
    for channel in 0..4 {
        mixed[channel] = left[channel] + (right[channel] - left[channel]) * amount;
    }
    mixed
}

fn edge(start: [f32; 2], end: [f32; 2], point: [f32; 2]) -> f32 {
    (end[0] - start[0]) * (point[1] - start[1]) - (end[1] - start[1]) * (point[0] - start[0])
}

pub(in crate::model) fn blend(
    destination: &mut [f32; 4],
    source: [f32; 4],
    mode: Moc3DrawableBlendMode,
) {
    match mode {
        Moc3DrawableBlendMode::Normal => {
            let inverse_alpha = 1.0 - source[3];
            for channel in 0..3 {
                destination[channel] = source[channel] + destination[channel] * inverse_alpha;
            }
            destination[3] = source[3] + destination[3] * inverse_alpha;
        }
        Moc3DrawableBlendMode::Additive => {
            for channel in 0..3 {
                destination[channel] += source[channel];
            }
        }
        Moc3DrawableBlendMode::Multiplicative => {
            for channel in 0..3 {
                destination[channel] *= source[channel] + 1.0 - source[3];
            }
        }
    }
}

fn to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
