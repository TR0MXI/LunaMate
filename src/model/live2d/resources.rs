//! 读取并冻结 Live2D 主体必需资源，供 Mocari 从同一份快照构造运行时。
//!
//! 模型文件属于不可信输入；路径边界、文件类型、实读字节和纹理解码都必须在本模块
//! 的固定预算内完成。校验成功后，Mocari 不再按清单路径打开任何主体必需资源。

use std::{
    error::Error,
    fmt, fs,
    io::{Cursor, ErrorKind, Read as _},
    path::{Path, PathBuf},
};

use image::{ImageFormat, ImageReader, Limits};
use mocari::{
    RuntimeModelAssets,
    assets::DecodedTexture,
    json::{Model3, Physics3, Pose3},
};

use super::super::{
    capabilities::{MAX_AUXILIARY_RESOURCE_BYTES, ModelResourceResolver},
    catalog::ModelManifest,
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MOC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TEXTURE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEXTURE_COUNT: usize = 16;
const MAX_TEXTURE_DIMENSION: u32 = 8_192;
const MAX_TOTAL_TEXTURE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_TEXTURE_DECODE_BYTES: u64 = MAX_TOTAL_TEXTURE_PIXELS * 4 + MAX_TEXTURE_FILE_BYTES;
const READ_CHUNK_BYTES: usize = 64 * 1024;

struct ResourceFileOpener<'a> {
    #[cfg(all(test, unix))]
    before_open: Option<&'a mut dyn FnMut(&Path)>,
    #[cfg(not(all(test, unix)))]
    _marker: std::marker::PhantomData<&'a mut ()>,
}

impl<'a> ResourceFileOpener<'a> {
    fn production() -> Self {
        Self {
            #[cfg(all(test, unix))]
            before_open: None,
            #[cfg(not(all(test, unix)))]
            _marker: std::marker::PhantomData,
        }
    }

    #[cfg(all(test, unix))]
    fn with_open_hook_for_test(before_open: &'a mut dyn FnMut(&Path)) -> Self {
        Self {
            before_open: Some(before_open),
        }
    }

    fn open_manifest(
        &mut self,
        manifest: &ModelManifest,
    ) -> Result<(ModelResourceResolver, fs::File), ResourceValidationError> {
        #[cfg(all(test, unix))]
        if let Some(before_open) = self.before_open.as_deref_mut() {
            return ModelResourceResolver::open_manifest_with_open_hook_for_test(
                manifest,
                MAX_MANIFEST_BYTES,
                before_open,
            )
            .map_err(|error| ResourceValidationError::new(format!("无法读取模型清单：{error}")));
        }
        ModelResourceResolver::open_manifest(manifest, MAX_MANIFEST_BYTES)
            .map_err(|error| ResourceValidationError::new(format!("无法读取模型清单：{error}")))
    }

    fn open_reference(
        &mut self,
        resolver: &ModelResourceResolver,
        reference: &str,
        label: &str,
        maximum_bytes: u64,
    ) -> Result<fs::File, ResourceValidationError> {
        #[cfg(all(test, unix))]
        if let Some(before_open) = self.before_open.as_deref_mut() {
            return resolver
                .open_file_with_open_hook_for_test(reference, maximum_bytes, before_open)
                .map_err(|error| {
                    ResourceValidationError::new(format!("{label} 引用 {reference} 无效：{error}"))
                });
        }
        resolver
            .open_file(reference, maximum_bytes)
            .map_err(|error| {
                ResourceValidationError::new(format!("{label} 引用 {reference} 无效：{error}"))
            })
    }
}

/// 同一 generation 的安全路径解析器与必需资源不可变快照。
#[derive(Debug)]
pub(in crate::model) struct ModelResourceSnapshot {
    resolver: ModelResourceResolver,
    assets: RuntimeModelAssets,
}

impl ModelResourceSnapshot {
    /// 把路径解析器和只读资产交给后续可选资源与主体运行时加载阶段。
    pub(in crate::model) fn into_parts(self) -> (ModelResourceResolver, RuntimeModelAssets) {
        (self.resolver, self.assets)
    }
}

/// 读取并校验模型主体必需资源，同时在分块读取边界检查取消状态。
pub(super) fn snapshot_model_resources(
    manifest: &ModelManifest,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<ModelResourceSnapshot, ResourceValidationError> {
    let mut opener = ResourceFileOpener::production();
    snapshot_model_resources_with_opener(manifest, &mut checkpoint, &mut opener)
}

fn snapshot_model_resources_with_opener(
    manifest: &ModelManifest,
    checkpoint: &mut dyn FnMut() -> bool,
    opener: &mut ResourceFileOpener<'_>,
) -> Result<ModelResourceSnapshot, ResourceValidationError> {
    check_cancelled(checkpoint)?;
    let (resolver, manifest_file) = opener.open_manifest(manifest)?;
    let path = manifest.path();
    let manifest_bytes = read_bounded_file(
        manifest_file,
        path,
        "模型清单",
        MAX_MANIFEST_BYTES,
        checkpoint,
    )?;
    let manifest_source = std::str::from_utf8(&manifest_bytes).map_err(|_| {
        ResourceValidationError::new(format!("模型清单不是有效 UTF-8：{}", path.display()))
    })?;
    let model = Model3::from_json_str(manifest_source).map_err(|_| {
        ResourceValidationError::new(format!("无法解析模型清单：{}", path.display()))
    })?;

    let moc = read_reference(
        &resolver,
        model.moc(),
        "MOC",
        MAX_MOC_BYTES,
        checkpoint,
        opener,
    )?
    .bytes;
    let textures = load_textures(&resolver, model.textures(), checkpoint, opener)?;
    let physics = model
        .physics()
        .map(|reference| {
            let resource = read_reference(
                &resolver,
                reference,
                "Physics",
                MAX_AUXILIARY_RESOURCE_BYTES,
                checkpoint,
                opener,
            )?;
            let source = std::str::from_utf8(&resource.bytes).map_err(|_| {
                ResourceValidationError::new(format!(
                    "Physics 不是有效 UTF-8：{}",
                    resource.path.display()
                ))
            })?;
            Physics3::from_json_str(source).map_err(|_| {
                ResourceValidationError::new(format!(
                    "无法解析 Physics：{}",
                    resource.path.display()
                ))
            })
        })
        .transpose()?;
    let pose = model
        .pose()
        .map(|reference| {
            let resource = read_reference(
                &resolver,
                reference,
                "Pose",
                MAX_AUXILIARY_RESOURCE_BYTES,
                checkpoint,
                opener,
            )?;
            let source = std::str::from_utf8(&resource.bytes).map_err(|_| {
                ResourceValidationError::new(format!(
                    "Pose 不是有效 UTF-8：{}",
                    resource.path.display()
                ))
            })?;
            Pose3::from_json_str(source).map_err(|_| {
                ResourceValidationError::new(format!("无法解析 Pose：{}", resource.path.display()))
            })
        })
        .transpose()?;
    check_cancelled(checkpoint)?;

    let model_dir = resolver.model_dir().to_path_buf();
    Ok(ModelResourceSnapshot {
        resolver,
        assets: RuntimeModelAssets::new(model, moc, physics, pose, textures, model_dir),
    })
}

/// 测试入口不注入取消，验证结果仍包含生产路径使用的完整快照。
#[cfg(test)]
pub(in crate::model) fn validate_model_resources(
    path: &Path,
) -> Result<ModelResourceSnapshot, ResourceValidationError> {
    let manifest = manifest_for_test(path)?;
    snapshot_model_resources(&manifest, || false)
}

/// 在每次最终路径打开前运行确定性替换，用于覆盖路径替换竞态。
#[cfg(all(test, unix))]
pub(in crate::model) fn snapshot_model_resources_with_open_hook_for_test(
    path: &Path,
    mut before_open: impl FnMut(&Path),
) -> Result<ModelResourceSnapshot, ResourceValidationError> {
    let manifest = manifest_for_test(path)?;
    let mut checkpoint = || false;
    let mut opener = ResourceFileOpener::with_open_hook_for_test(&mut before_open);
    snapshot_model_resources_with_opener(&manifest, &mut checkpoint, &mut opener)
}

#[cfg(test)]
fn manifest_for_test(path: &Path) -> Result<ModelManifest, ResourceValidationError> {
    ModelManifest::for_path_for_test(path).map_err(|error| {
        ResourceValidationError::new(format!(
            "无法建立测试模型根快照 {}：{error}",
            path.display()
        ))
    })
}

#[derive(Debug)]
struct RequiredFileSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
}

fn read_reference(
    resolver: &ModelResourceResolver,
    reference: &str,
    label: &str,
    maximum_bytes: u64,
    checkpoint: &mut dyn FnMut() -> bool,
    opener: &mut ResourceFileOpener<'_>,
) -> Result<RequiredFileSnapshot, ResourceValidationError> {
    check_cancelled(checkpoint)?;
    let file = opener.open_reference(resolver, reference, label, maximum_bytes)?;
    let path = resolver.model_dir().join(reference);
    let resource_label = format!("{label} 引用 {reference}");
    let bytes = read_bounded_file(file, &path, &resource_label, maximum_bytes, checkpoint)?;
    Ok(RequiredFileSnapshot { path, bytes })
}

fn read_bounded_file(
    file: fs::File,
    path: &Path,
    label: &str,
    maximum_bytes: u64,
    checkpoint: &mut dyn FnMut() -> bool,
) -> Result<Vec<u8>, ResourceValidationError> {
    check_cancelled(checkpoint)?;
    let metadata = file.metadata().map_err(|_| {
        ResourceValidationError::new(format!("无法读取{label}元数据：{}", path.display()))
    })?;
    validate_file_metadata(&metadata, path, label, maximum_bytes)?;
    check_cancelled(checkpoint)?;

    let read_limit = maximum_bytes
        .checked_add(1)
        .ok_or_else(|| ResourceValidationError::new(format!("{label}读取上限发生整数溢出")))?;
    let maximum_capacity = usize::try_from(maximum_bytes).map_err(|_| {
        ResourceValidationError::new(format!("{label}读取上限无法表示为当前平台内存大小"))
    })?;
    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(maximum_capacity)
        .min(maximum_capacity);
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(initial_capacity).map_err(|error| {
        ResourceValidationError::new(format!("无法分配{label}读取缓冲：{error}"))
    })?;

    let mut reader = file.take(read_limit);
    loop {
        check_cancelled(checkpoint)?;
        let remaining = read_limit.saturating_sub(bytes.len() as u64);
        if remaining == 0 {
            break;
        }
        let chunk_size = usize::try_from(remaining.min(READ_CHUNK_BYTES as u64))
            .map_err(|_| ResourceValidationError::new(format!("{label}读取块大小无法表示")))?;
        let start = bytes.len();
        bytes.try_reserve_exact(chunk_size).map_err(|error| {
            ResourceValidationError::new(format!("无法扩展{label}读取缓冲：{error}"))
        })?;
        bytes.resize(start + chunk_size, 0);
        let read = loop {
            match reader.read(&mut bytes[start..start + chunk_size]) {
                Ok(read) => break read,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    return Err(ResourceValidationError::new(format!(
                        "无法读取{label}：{}",
                        path.display()
                    )));
                }
            }
        };
        bytes.truncate(start + read);
        if read == 0 {
            break;
        }
    }
    if bytes.len() as u64 > maximum_bytes {
        return Err(ResourceValidationError::new(format!(
            "{label}实际读取大小 {} 字节超过上限 {maximum_bytes}：{}",
            bytes.len(),
            path.display()
        )));
    }
    check_cancelled(checkpoint)?;
    Ok(bytes)
}

fn validate_file_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    label: &str,
    maximum_bytes: u64,
) -> Result<(), ResourceValidationError> {
    if !metadata.file_type().is_file() {
        return Err(ResourceValidationError::new(format!(
            "{label}不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > maximum_bytes {
        return Err(ResourceValidationError::new(format!(
            "{label}大小 {} 字节超过上限 {maximum_bytes}：{}",
            metadata.len(),
            path.display()
        )));
    }
    Ok(())
}

fn load_textures(
    resolver: &ModelResourceResolver,
    references: &[String],
    checkpoint: &mut dyn FnMut() -> bool,
    opener: &mut ResourceFileOpener<'_>,
) -> Result<Vec<DecodedTexture>, ResourceValidationError> {
    if references.len() > MAX_TEXTURE_COUNT {
        return Err(ResourceValidationError::new(format!(
            "纹理数量 {} 超过上限 {MAX_TEXTURE_COUNT}",
            references.len()
        )));
    }

    let mut textures = Vec::new();
    textures
        .try_reserve_exact(references.len())
        .map_err(|error| ResourceValidationError::new(format!("无法分配纹理快照：{error}")))?;
    let mut total_texture_pixels = 0_u64;
    for (index, reference) in references.iter().enumerate() {
        let label = format!("纹理 {index}");
        let resource = read_reference(
            resolver,
            reference,
            &label,
            MAX_TEXTURE_FILE_BYTES,
            checkpoint,
            opener,
        )?;
        let (format, width, height) = inspect_texture(&resource.bytes, &resource.path)?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| ResourceValidationError::new("纹理像素数量发生整数溢出"))?;
        total_texture_pixels = total_texture_pixels
            .checked_add(pixels)
            .ok_or_else(|| ResourceValidationError::new("纹理总像素数量发生整数溢出"))?;
        if total_texture_pixels > MAX_TOTAL_TEXTURE_PIXELS {
            return Err(ResourceValidationError::new(format!(
                "纹理总像素数量 {total_texture_pixels} 超过上限 {MAX_TOTAL_TEXTURE_PIXELS}"
            )));
        }

        check_cancelled(checkpoint)?;
        textures.push(decode_texture(
            &resource.bytes,
            &resource.path,
            format,
            width,
            height,
        )?);
        check_cancelled(checkpoint)?;
    }
    Ok(textures)
}

fn inspect_texture(
    bytes: &[u8],
    path: &Path,
) -> Result<(ImageFormat, u32, u32), ResourceValidationError> {
    let format = image::guess_format(bytes).map_err(|_| {
        ResourceValidationError::new(format!("无法读取纹理尺寸：{}", path.display()))
    })?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(texture_limits());
    let (width, height) = reader.into_dimensions().map_err(|_| {
        ResourceValidationError::new(format!(
            "无法读取纹理尺寸，或尺寸超过单边上限 {MAX_TEXTURE_DIMENSION}：{}",
            path.display()
        ))
    })?;
    if width == 0 || height == 0 || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION
    {
        return Err(ResourceValidationError::new(format!(
            "纹理 {} 尺寸 {width}x{height} 超过单边上限 {MAX_TEXTURE_DIMENSION}",
            path.display()
        )));
    }
    Ok((format, width, height))
}

fn decode_texture(
    bytes: &[u8],
    path: &Path,
    format: ImageFormat,
    width: u32,
    height: u32,
) -> Result<DecodedTexture, ResourceValidationError> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(texture_limits());
    let image = reader
        .decode()
        .map_err(|_| ResourceValidationError::new(format!("无法解码纹理：{}", path.display())))?;
    if image.width() != width || image.height() != height {
        return Err(ResourceValidationError::new(format!(
            "纹理解码尺寸与已校验尺寸不一致：{}",
            path.display()
        )));
    }
    let expected_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| ResourceValidationError::new("纹理 RGBA 大小发生整数溢出"))?;
    let rgba = image.into_rgba8().into_raw();
    if rgba.len() != expected_bytes {
        return Err(ResourceValidationError::new(format!(
            "纹理 RGBA 数据长度无效：{}",
            path.display()
        )));
    }
    Ok(DecodedTexture::new(width, height, rgba))
}

fn texture_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_TEXTURE_DIMENSION);
    limits.max_image_height = Some(MAX_TEXTURE_DIMENSION);
    limits.max_alloc = Some(MAX_TEXTURE_DECODE_BYTES);
    limits
}

fn check_cancelled(checkpoint: &mut dyn FnMut() -> bool) -> Result<(), ResourceValidationError> {
    if checkpoint() {
        Err(ResourceValidationError::Cancelled)
    } else {
        Ok(())
    }
}

/// 测试文件增长时复用生产读取循环，不为测试放宽生产上限判断。
#[cfg(test)]
pub(in crate::model) fn read_bounded_file_for_test(
    path: &Path,
    maximum_bytes: u64,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<Vec<u8>, ResourceValidationError> {
    let (_, file) = ModelResourceResolver::open_unscanned_manifest_for_test(path, maximum_bytes)
        .map_err(|error| ResourceValidationError::new(format!("无法读取测试资源：{error}")))?;
    read_bounded_file(file, path, "测试资源", maximum_bytes, &mut checkpoint)
}

/// 描述模型资源快照建立失败。
#[derive(Debug)]
pub(crate) enum ResourceValidationError {
    Cancelled,
    Invalid { message: String },
}

impl ResourceValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self::Invalid {
            message: message.into(),
        }
    }

    /// 返回失败是否只表示当前 generation 已失效。
    pub(super) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for ResourceValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("模型资源读取已取消"),
            Self::Invalid { message } => formatter.write_str(message),
        }
    }
}

impl Error for ResourceValidationError {}
