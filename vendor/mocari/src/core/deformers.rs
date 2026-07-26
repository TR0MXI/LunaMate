use super::math::{Vector2, degrees_to_radian};

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DeformerTransform<'a> {
    Rotation {
        angle_degrees: f32,
        scale: f32,
        translation: Vector2,
        flip_x: bool,
        flip_y: bool,
    },
    Warp {
        grid: &'a [Vector2],
        cols: usize,
        rows: usize,
        interpolation: WarpInterpolation,
    },
}

/// 预计算旋转变形的 2×2 矩阵与平移。
///
/// 旋转矩阵只取决于变形器自身的角度、缩放和翻转，与被变换的顶点无关。批量
/// 变换同一变形器下的顶点时先构造一次，可以避免每个顶点重复求三角函数。
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct RotationTarget {
    m00: f32,
    m01: f32,
    m10: f32,
    m11: f32,
    translation: Vector2,
}

impl RotationTarget {
    pub fn new(
        angle_degrees: f32,
        scale: f32,
        translation: Vector2,
        flip_x: bool,
        flip_y: bool,
    ) -> Self {
        let theta = degrees_to_radian(angle_degrees);
        let cos = theta.cos();
        let sin = theta.sin();
        let sign_x = if flip_x { -1.0 } else { 1.0 };
        let sign_y = if flip_y { -1.0 } else { 1.0 };

        Self {
            m00: cos * scale * sign_x,
            m01: -sin * scale * sign_y,
            m10: sin * scale * sign_x,
            m11: cos * scale * sign_y,
            translation,
        }
    }

    pub fn transform(&self, point: Vector2) -> Vector2 {
        Vector2::new(
            self.m00 * point.x() + self.m01 * point.y() + self.translation.x(),
            self.m10 * point.x() + self.m11 * point.y() + self.translation.y(),
        )
    }

    pub fn transform_slice(&self, points: &mut [Vector2]) {
        for point in points {
            *point = self.transform(*point);
        }
    }
}

pub fn rotation_deformer_transform_point(
    point: Vector2,
    angle_degrees: f32,
    scale: f32,
    translation: Vector2,
    flip_x: bool,
    flip_y: bool,
) -> Vector2 {
    RotationTarget::new(angle_degrees, scale, translation, flip_x, flip_y).transform(point)
}

pub fn transform_art_mesh_vertices_by_deformers(
    vertices: &[Vector2],
    transforms: &[DeformerTransform<'_>],
) -> Option<Vec<Vector2>> {
    let mut out = vertices.to_vec();

    for transform in transforms {
        // 顶点为空时不构造变形目标，保持与逐顶点实现一致：空网格不会因为
        // 变形器数据非法而失败。
        if out.is_empty() {
            break;
        }
        match *transform {
            DeformerTransform::Rotation {
                angle_degrees,
                scale,
                translation,
                flip_x,
                flip_y,
            } => RotationTarget::new(angle_degrees, scale, translation, flip_x, flip_y)
                .transform_slice(&mut out),
            DeformerTransform::Warp {
                grid,
                cols,
                rows,
                interpolation,
            } => WarpTarget::new(grid, cols, rows, interpolation)?.transform_slice(&mut out)?,
        }
    }

    Some(out)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WarpInterpolation {
    Quad,
    Triangle,
}

/// 预校验后的 warp 变形目标。
///
/// `stride`、网格长度校验和外插基底都只取决于变形器网格本身，与被变换的顶点
/// 无关。批量变换同一网格下的顶点时先构造一次，可以把这些不变量从每顶点路径
/// 中移除；`warp_deformer_transform_target` 保留原有的单点可失败接口。
#[derive(Debug, Copy, Clone)]
pub struct WarpTarget<'a> {
    grid: &'a [Vector2],
    cols: usize,
    rows: usize,
    stride: usize,
    interpolation: WarpInterpolation,
    basis: WarpExtrapBasis,
}

impl<'a> WarpTarget<'a> {
    /// 校验网格几何并预计算外插基底；网格无法表示 `cols`×`rows` 单元时返回 `None`。
    pub fn new(
        grid: &'a [Vector2],
        cols: usize,
        rows: usize,
        interpolation: WarpInterpolation,
    ) -> Option<Self> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let stride = cols.checked_add(1)?;
        let required = stride.checked_mul(rows.checked_add(1)?)?;
        if grid.len() < required {
            return None;
        }

        Some(Self {
            grid,
            cols,
            rows,
            stride,
            interpolation,
            // 四角只在网格更新时变化，外插顶点因此不必各自重建基底。
            basis: WarpExtrapBasis::from_corners(grid, rows, cols, stride),
        })
    }

    pub fn transform(&self, local_point: Vector2) -> Option<Vector2> {
        let (x, y) = (local_point.x(), local_point.y());
        if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) {
            return interpolate_inside(
                local_point,
                self.grid,
                self.cols,
                self.rows,
                self.stride,
                self.interpolation,
            );
        }

        if !(-2.0..3.0).contains(&x) || !(-2.0..3.0).contains(&y) {
            return Some(Vector2::new(
                self.basis.dpdv.x() * x + self.basis.center.x() + self.basis.dpdu.x() * y,
                self.basis.dpdv.y() * x + self.basis.center.y() + self.basis.dpdu.y() * y,
            ));
        }

        let cell = self.basis.extrap_cell(
            x,
            y,
            x * self.cols as f32,
            y * self.rows as f32,
            self.rows,
            self.cols,
            self.stride,
            self.grid,
        );
        Some(triangle_interpolate(&cell))
    }

    pub fn transform_slice(&self, points: &mut [Vector2]) -> Option<()> {
        for point in points {
            *point = self.transform(*point)?;
        }
        Some(())
    }
}

/// 在调用者已校验 `stride` 与网格长度后定位单元并插值。
fn interpolate_inside(
    local_point: Vector2,
    grid: &[Vector2],
    cols: usize,
    rows: usize,
    stride: usize,
    interpolation: WarpInterpolation,
) -> Option<Vector2> {
    let u = local_point.x() * cols as f32;
    let v = local_point.y() * rows as f32;
    let i = u.trunc() as usize;
    let j = v.trunc() as usize;
    let s = u - i as f32;
    let t = v - j as f32;

    if i >= cols || j >= rows {
        return None;
    }

    let c00 = grid[j * stride + i];
    let c10 = grid[j * stride + i + 1];
    let c01 = grid[(j + 1) * stride + i];
    let c11 = grid[(j + 1) * stride + i + 1];

    Some(match interpolation {
        WarpInterpolation::Quad => bilinear_cell(s, t, c00, c10, c01, c11),
        WarpInterpolation::Triangle => triangle_cell(s, t, c00, c10, c01, c11),
    })
}

pub fn warp_deformer_transform_inside(
    local_point: Vector2,
    grid: &[Vector2],
    cols: usize,
    rows: usize,
    interpolation: WarpInterpolation,
) -> Option<Vector2> {
    if !(0.0..1.0).contains(&local_point.x()) || !(0.0..1.0).contains(&local_point.y()) {
        return None;
    }

    let stride = cols.checked_add(1)?;
    let required = stride.checked_mul(rows.checked_add(1)?)?;
    if grid.len() < required {
        return None;
    }

    interpolate_inside(local_point, grid, cols, rows, stride, interpolation)
}

pub fn warp_deformer_transform_target(
    local_point: Vector2,
    grid: &[Vector2],
    cols: usize,
    rows: usize,
    interpolation: WarpInterpolation,
) -> Option<Vector2> {
    WarpTarget::new(grid, cols, rows, interpolation)?.transform(local_point)
}

struct WarpCell {
    fu: f32,
    fv: f32,
    p00: Vector2,
    p10: Vector2,
    p01: Vector2,
    p11: Vector2,
}

#[derive(Debug, Copy, Clone)]
struct WarpExtrapBasis {
    center: Vector2,
    dpdu: Vector2,
    dpdv: Vector2,
}

impl WarpExtrapBasis {
    fn from_corners(grid: &[Vector2], rows: usize, cols: usize, stride: usize) -> Self {
        let c00 = grid[0];
        let c10 = grid[cols];
        let c01 = grid[rows * stride];
        let c11 = grid[rows * stride + cols];

        let d11_00 = sub(c11, c00);
        let d10_01 = sub(c10, c01);

        let dpdu = scale(sub(d11_00, d10_01), 0.5);
        let dpdv = scale(add(d10_01, d11_00), 0.5);
        let sum = add(add(c00, c10), add(c01, c11));
        let center = sub(scale(sum, 0.25), scale(d11_00, 0.5));

        Self { center, dpdu, dpdv }
    }

    #[allow(clippy::too_many_arguments)]
    fn extrap_cell(
        &self,
        x: f32,
        y: f32,
        gu: f32,
        gv: f32,
        rows: usize,
        cols: usize,
        stride: usize,
        grid: &[Vector2],
    ) -> WarpCell {
        let (fr, fc) = (rows as f32, cols as f32);
        let (cen, du, dv) = (self.center, self.dpdu, self.dpdv);

        if x <= 0.0 {
            if y <= 0.0 {
                WarpCell {
                    fu: (x + 2.0) * 0.5,
                    fv: (y + 2.0) * 0.5,
                    p00: sub(cen, add(scale(du, 2.0), scale(dv, 2.0))),
                    p10: sub(cen, scale(du, 2.0)),
                    p01: sub(cen, scale(dv, 2.0)),
                    p11: grid[0],
                }
            } else if y < 1.0 {
                let cv = clamp_cell(gv as i32, rows);
                let vc = cv as f32 / fr;
                let vn = (cv + 1) as f32 / fr;
                WarpCell {
                    fu: (x + 2.0) * 0.5,
                    fv: gv - cv as f32,
                    p00: add(sub(cen, scale(dv, 2.0)), scale(du, vc)),
                    p10: grid[cv as usize * stride],
                    p01: add(sub(cen, scale(dv, 2.0)), scale(du, vn)),
                    p11: grid[(cv + 1) as usize * stride],
                }
            } else {
                WarpCell {
                    fu: (x + 2.0) * 0.5,
                    fv: (y - 1.0) * 0.5,
                    p00: add(sub(cen, scale(dv, 2.0)), du),
                    p10: grid[rows * stride],
                    p01: add(sub(cen, scale(dv, 2.0)), scale(du, 3.0)),
                    p11: add(cen, scale(du, 3.0)),
                }
            }
        } else if x < 1.0 {
            let cu = clamp_cell(gu as i32, cols);
            let uc = cu as f32 / fc;
            let un = (cu + 1) as f32 / fc;
            if y <= 0.0 {
                WarpCell {
                    fu: gu - cu as f32,
                    fv: (y + 2.0) * 0.5,
                    p00: add(scale(dv, uc), sub(cen, scale(du, 2.0))),
                    p10: add(scale(dv, un), sub(cen, scale(du, 2.0))),
                    p01: grid[cu as usize],
                    p11: grid[cu as usize + 1],
                }
            } else {
                WarpCell {
                    fu: gu - cu as f32,
                    fv: (y - 1.0) * 0.5,
                    p00: grid[rows * stride + cu as usize],
                    p10: grid[rows * stride + cu as usize + 1],
                    p01: add(add(cen, scale(dv, uc)), scale(du, 3.0)),
                    p11: add(add(cen, scale(dv, un)), scale(du, 3.0)),
                }
            }
        } else if y <= 0.0 {
            WarpCell {
                fu: (x - 1.0) * 0.5,
                fv: (y + 2.0) * 0.5,
                p00: add(sub(cen, scale(du, 2.0)), dv),
                p10: add(sub(cen, scale(du, 2.0)), scale(dv, 3.0)),
                p01: grid[cols],
                p11: add(cen, scale(dv, 3.0)),
            }
        } else if y < 1.0 {
            let cv = clamp_cell(gv as i32, rows);
            let vc = cv as f32 / fr;
            let vn = (cv + 1) as f32 / fr;
            WarpCell {
                fu: (x - 1.0) * 0.5,
                fv: gv - cv as f32,
                p00: grid[cols + cv as usize * stride],
                p10: add(add(cen, scale(dv, 3.0)), scale(du, vc)),
                p01: grid[cols + (cv + 1) as usize * stride],
                p11: add(add(cen, scale(dv, 3.0)), scale(du, vn)),
            }
        } else {
            WarpCell {
                fu: (x - 1.0) * 0.5,
                fv: (y - 1.0) * 0.5,
                p00: grid[rows * stride + cols],
                p10: add(add(cen, scale(dv, 3.0)), du),
                p01: add(add(cen, scale(du, 3.0)), dv),
                p11: add(cen, add(scale(dv, 3.0), scale(du, 3.0))),
            }
        }
    }
}

fn clamp_cell(cell: i32, count: usize) -> i32 {
    if cell == count as i32 { cell - 1 } else { cell }
}

fn triangle_interpolate(cell: &WarpCell) -> Vector2 {
    let (fu, fv) = (cell.fu, cell.fv);
    if fu + fv <= 1.0 {
        bary3(cell.p00, cell.p10, cell.p01, 1.0 - fu - fv, fu, fv)
    } else {
        bary3(
            cell.p10,
            cell.p11,
            cell.p01,
            1.0 - fv,
            fu + fv - 1.0,
            1.0 - fu,
        )
    }
}

fn bary3(a: Vector2, b: Vector2, c: Vector2, wa: f32, wb: f32, wc: f32) -> Vector2 {
    add(add(scale(a, wa), scale(b, wb)), scale(c, wc))
}

fn add(a: Vector2, b: Vector2) -> Vector2 {
    Vector2::new(a.x() + b.x(), a.y() + b.y())
}

fn sub(a: Vector2, b: Vector2) -> Vector2 {
    Vector2::new(a.x() - b.x(), a.y() - b.y())
}

fn scale(a: Vector2, s: f32) -> Vector2 {
    Vector2::new(a.x() * s, a.y() * s)
}

fn bilinear_cell(
    s: f32,
    t: f32,
    c00: Vector2,
    c10: Vector2,
    c01: Vector2,
    c11: Vector2,
) -> Vector2 {
    c00.lerp(c10, s).lerp(c01.lerp(c11, s), t)
}

fn triangle_cell(
    s: f32,
    t: f32,
    c00: Vector2,
    c10: Vector2,
    c01: Vector2,
    c11: Vector2,
) -> Vector2 {
    if s + t <= 1.0 {
        return Vector2::affine2(c00, c10, s, c01, t);
    }

    let a = 1.0 - s;
    let b = 1.0 - t;
    Vector2::affine2(c11, c01, a, c10, b)
}
