//! 保存与已渲染图像一致的 HitArea 快照，并执行窗口坐标命中检测。

use std::sync::Arc;

use gpui::RenderImage;
use mocari::moc3::Moc3DrawableMesh;

use crate::capabilities::HitAreaCapability;

use super::HitAreaActivation;

/// 与一张已显示模型图像保持同步的 HitArea。
#[derive(Clone, Debug)]
pub(crate) struct RenderedHitArea {
    id: Arc<str>,
    name: Arc<str>,
    bounds: RasterBounds,
}

impl RenderedHitArea {
    /// 从光栅坐标包围盒创建命中区域；非法坐标会被拒绝。
    pub(crate) fn new(id: Arc<str>, name: Arc<str>, bounds: [f32; 4]) -> Option<Self> {
        Some(Self {
            id,
            name,
            bounds: RasterBounds::new(bounds)?,
        })
    }

    /// 返回可发送给后台模型的语义激活事件。
    pub(crate) fn activation(&self) -> HitAreaActivation {
        HitAreaActivation::new(self.id.clone(), self.name.clone())
    }

    /// 判断一个光栅坐标点是否位于当前包围盒内。
    fn contains(&self, point: [f32; 2]) -> bool {
        self.bounds.contains(point)
    }

    #[cfg(test)]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// 一次帧结果及其同一运行时状态下生成的命中区域快照。
pub(crate) struct RenderedModelFrame {
    image: Option<Arc<RenderImage>>,
    hit_areas: Arc<[RenderedHitArea]>,
    raster_dimensions: [u32; 2],
}

impl RenderedModelFrame {
    /// 组合渲染图像、HitArea 和实际离屏光栅尺寸。
    pub(crate) fn new(
        image: RenderImage,
        hit_areas: Vec<RenderedHitArea>,
        raster_dimensions: [u32; 2],
    ) -> Self {
        Self {
            image: Some(Arc::new(image)),
            hit_areas: hit_areas.into(),
            raster_dimensions,
        }
    }

    /// 创建已经由原生 GPU underlay 呈现、无需 GPUI 图像载荷的帧快照。
    pub(crate) fn gpu(hit_areas: Vec<RenderedHitArea>, raster_dimensions: [u32; 2]) -> Self {
        Self {
            image: None,
            hit_areas: hit_areas.into(),
            raster_dimensions,
        }
    }

    /// 返回 CPU 回退路径中可交给 GPUI 图像元素的图像。
    pub(crate) fn image(&self) -> Option<&Arc<RenderImage>> {
        self.image.as_ref()
    }

    /// 使用当前窗口逻辑尺寸检测与本帧画面一致的 HitArea。
    pub(crate) fn hit_area_at_window_point(
        &self,
        position: [f32; 2],
        viewport: [f32; 2],
    ) -> Option<&RenderedHitArea> {
        let raster_point = if self.image.is_some() {
            window_point_to_raster(position, viewport, self.raster_dimensions)?
        } else {
            window_point_to_stretched_raster(position, viewport, self.raster_dimensions)?
        };
        self.hit_areas
            .iter()
            .find(|hit_area| hit_area.contains(raster_point))
    }

    #[cfg(test)]
    pub(crate) fn hit_areas(&self) -> &[RenderedHitArea] {
        &self.hit_areas
    }
}

/// 根据当前帧 Drawable 顶点生成与渲染图像一致的 HitArea 包围盒。
pub(crate) fn render_hit_areas(
    hit_areas: &[HitAreaCapability],
    bounds_count: usize,
    meshes: &[Moc3DrawableMesh],
    mut transform: impl FnMut([f32; 2]) -> [f32; 2],
) -> Vec<RenderedHitArea> {
    if hit_areas.is_empty() {
        return Vec::new();
    }
    // 同一 Drawable 可能被多个语义区域复用；每帧只扫描和转换一次顶点。
    let mut bounds_by_drawable = vec![DrawableBounds::Pending; bounds_count];
    hit_areas
        .iter()
        .filter_map(|hit_area| {
            let drawable_index = hit_area.drawable_index();
            let bounds_slot = hit_area.bounds_slot();
            let mesh = meshes.get(drawable_index)?;
            let bounds = match *bounds_by_drawable.get(bounds_slot)? {
                DrawableBounds::Visible(bounds) => bounds,
                DrawableBounds::Hidden => return None,
                DrawableBounds::Pending => match drawable_bounds(mesh, &mut transform) {
                    Some(bounds) => {
                        bounds_by_drawable[bounds_slot] = DrawableBounds::Visible(bounds);
                        bounds
                    }
                    None => {
                        bounds_by_drawable[bounds_slot] = DrawableBounds::Hidden;
                        return None;
                    }
                },
            };
            RenderedHitArea::new(hit_area.id().clone(), hit_area.name().clone(), bounds)
        })
        .collect()
}

#[derive(Clone, Copy)]
enum DrawableBounds {
    Pending,
    Hidden,
    Visible([f32; 4]),
}

fn drawable_bounds(
    mesh: &Moc3DrawableMesh,
    transform: &mut impl FnMut([f32; 2]) -> [f32; 2],
) -> Option<[f32; 4]> {
    let opacity = mesh.opacity();
    if !opacity.is_finite() || opacity <= 0.0 {
        return None;
    }

    let mut bounds: Option<[f32; 4]> = None;
    for vertex in mesh.vertices() {
        let [x, y] = transform(vertex.position());
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        bounds = Some(match bounds {
            Some([min_x, min_y, max_x, max_y]) => {
                [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
            }
            None => [x, y, x, y],
        });
    }
    bounds
}

#[derive(Clone, Copy, Debug)]
struct RasterBounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl RasterBounds {
    fn new([min_x, min_y, max_x, max_y]: [f32; 4]) -> Option<Self> {
        let coordinates = [min_x, min_y, max_x, max_y];
        if coordinates.iter().any(|coordinate| !coordinate.is_finite())
            || min_x > max_x
            || min_y > max_y
        {
            return None;
        }

        Some(Self {
            min_x,
            min_y,
            max_x,
            max_y,
        })
    }

    fn contains(self, [x, y]: [f32; 2]) -> bool {
        x.is_finite()
            && y.is_finite()
            && (self.min_x..=self.max_x).contains(&x)
            && (self.min_y..=self.max_y).contains(&y)
    }
}

/// 将窗口逻辑坐标转换为 `ObjectFit::Contain` 图像中的光栅坐标。
fn window_point_to_raster(
    [position_x, position_y]: [f32; 2],
    [viewport_width, viewport_height]: [f32; 2],
    [raster_width, raster_height]: [u32; 2],
) -> Option<[f32; 2]> {
    let values = [position_x, position_y, viewport_width, viewport_height];
    if values.iter().any(|value| !value.is_finite())
        || viewport_width <= 0.0
        || viewport_height <= 0.0
        || raster_width == 0
        || raster_height == 0
    {
        return None;
    }

    let raster_width = raster_width as f32;
    let raster_height = raster_height as f32;
    let image_ratio = raster_width / raster_height;
    let viewport_ratio = viewport_width / viewport_height;
    let (origin_x, origin_y, painted_width, painted_height) = if viewport_ratio > image_ratio {
        let painted_width = viewport_height * image_ratio;
        (
            (viewport_width - painted_width) * 0.5,
            0.0,
            painted_width,
            viewport_height,
        )
    } else {
        let painted_height = viewport_width / image_ratio;
        (
            0.0,
            (viewport_height - painted_height) * 0.5,
            viewport_width,
            painted_height,
        )
    };

    let normalized_x = (position_x - origin_x) / painted_width;
    let normalized_y = (position_y - origin_y) / painted_height;
    if !(0.0..=1.0).contains(&normalized_x) || !(0.0..=1.0).contains(&normalized_y) {
        return None;
    }

    Some([normalized_x * raster_width, normalized_y * raster_height])
}

/// 将窗口逻辑坐标按原生 surface 的独立双轴缩放映射到物理像素。
fn window_point_to_stretched_raster(
    [position_x, position_y]: [f32; 2],
    [viewport_width, viewport_height]: [f32; 2],
    [raster_width, raster_height]: [u32; 2],
) -> Option<[f32; 2]> {
    let values = [position_x, position_y, viewport_width, viewport_height];
    if values.iter().any(|value| !value.is_finite())
        || viewport_width <= 0.0
        || viewport_height <= 0.0
        || raster_width == 0
        || raster_height == 0
    {
        return None;
    }
    let normalized_x = position_x / viewport_width;
    let normalized_y = position_y / viewport_height;
    if !(0.0..=1.0).contains(&normalized_x) || !(0.0..=1.0).contains(&normalized_y) {
        return None;
    }
    Some([
        normalized_x * raster_width as f32,
        normalized_y * raster_height as f32,
    ])
}

#[cfg(test)]
mod tests {
    use image::{Frame, RgbaImage};

    use super::*;

    #[test]
    fn maps_matching_aspect_ratio_to_raster_coordinates() {
        assert_eq!(
            window_point_to_raster([180.0, 320.0], [360.0, 640.0], [720, 1280]),
            Some([360.0, 640.0])
        );
    }

    #[test]
    fn rejects_horizontal_contain_letterbox() {
        assert_eq!(
            window_point_to_raster([249.0, 300.0], [800.0, 600.0], [300, 600]),
            None
        );
        assert_eq!(
            window_point_to_raster([400.0, 300.0], [800.0, 600.0], [300, 600]),
            Some([150.0, 300.0])
        );
    }

    #[test]
    fn rejects_vertical_contain_letterbox() {
        assert_eq!(
            window_point_to_raster([150.0, 99.0], [300.0, 800.0], [300, 600]),
            None
        );
        assert_eq!(
            window_point_to_raster([150.0, 400.0], [300.0, 800.0], [300, 600]),
            Some([150.0, 300.0])
        );
    }

    #[test]
    fn hit_area_uses_inclusive_raster_bounds() {
        let area = RenderedHitArea::new(
            Arc::from("HitArea"),
            Arc::from("Body"),
            [10.0, 20.0, 30.0, 40.0],
        )
        .expect("finite bounds should create a hit area");

        assert!(area.contains([10.0, 20.0]));
        assert!(area.contains([30.0, 40.0]));
        assert!(!area.contains([30.1, 40.0]));
    }

    #[test]
    fn invalid_dimensions_and_coordinates_are_rejected() {
        assert_eq!(
            window_point_to_raster([0.0, 0.0], [0.0, 640.0], [360, 640]),
            None
        );
        assert_eq!(
            window_point_to_raster([f32::NAN, 0.0], [360.0, 640.0], [360, 640]),
            None
        );
        assert!(
            RenderedHitArea::new(
                Arc::from("HitArea"),
                Arc::from("Body"),
                [f32::NAN, 0.0, 1.0, 1.0],
            )
            .is_none()
        );
    }

    #[test]
    fn frame_hit_test_preserves_model_declaration_order() {
        let areas = vec![
            RenderedHitArea::new(
                Arc::from("BodyDrawable"),
                Arc::from("Body"),
                [20.0, 20.0, 80.0, 80.0],
            )
            .expect("body bounds should be valid"),
            RenderedHitArea::new(
                Arc::from("HeadDrawable"),
                Arc::from("Head"),
                [40.0, 40.0, 60.0, 60.0],
            )
            .expect("head bounds should be valid"),
        ];
        let image = RenderImage::new(vec![Frame::new(RgbaImage::new(100, 100))]);
        let frame = RenderedModelFrame::new(image, areas, [100, 100]);

        let hit = frame
            .hit_area_at_window_point([50.0, 50.0], [100.0, 100.0])
            .expect("overlapping point should hit the first declared area");
        assert_eq!(hit.name(), "Body");
        assert!(
            frame
                .hit_area_at_window_point([5.0, 5.0], [100.0, 100.0])
                .is_none()
        );
    }

    #[test]
    fn gpu_frame_keeps_hit_testing_without_a_gpui_image() {
        let area = RenderedHitArea::new(
            Arc::from("BodyDrawable"),
            Arc::from("Body"),
            [10.0, 10.0, 90.0, 90.0],
        )
        .expect("GPU 帧命中区域应有效");
        let frame = RenderedModelFrame::gpu(vec![area], [100, 100]);

        assert!(frame.image().is_none());
        assert!(
            frame
                .hit_area_at_window_point([50.0, 50.0], [100.0, 100.0])
                .is_some()
        );
    }

    #[test]
    fn gpu_frame_maps_each_surface_axis_without_contain_letterboxing() {
        assert_eq!(
            window_point_to_stretched_raster([150.0, 250.0], [300.0, 500.0], [375, 626]),
            Some([187.5, 313.0])
        );
    }
}
