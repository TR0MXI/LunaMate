//! 在 Mocari 读取模型前校验外部资源边界与解码预算。
//!
//! 模型文件属于不可信输入；本模块不解析运行时数据，只阻止路径逃逸和明显超限资源。

use std::{
    error::Error,
    fmt, fs,
    io::Read as _,
    path::{Path, PathBuf},
};

use mocari::json::Model3;

use super::super::capabilities::{MAX_AUXILIARY_RESOURCE_BYTES, ModelResourceResolver};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MOC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TEXTURE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEXTURE_COUNT: usize = 16;
const MAX_TEXTURE_DIMENSION: u32 = 8_192;
const MAX_TOTAL_TEXTURE_PIXELS: u64 = 64 * 1024 * 1024;

/// 校验模型主体必需资源，并返回可选资源必须复用的安全路径解析器。
///
/// # Errors
///
/// 清单、MOC、纹理、Physics 或 Pose 无法安全读取时返回错误。动作、表情和 DisplayInfo
/// 不在主体预检阶段读取，它们应通过返回的解析器逐项处理。
pub(in crate::model) fn validate_model_resources(
    path: &Path,
) -> Result<ModelResourceResolver, ResourceValidationError> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        // Windows 的 CreateFileW 未带 FILE_FLAG_BACKUP_SEMANTICS 时无法为目录返回句柄，
        // 打开阶段就会失败；此处补一次 stat，避免把类型错误误报成读取失败。
        Err(_) => {
            let not_regular_file =
                fs::metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file());
            return Err(ResourceValidationError::new(if not_regular_file {
                format!("模型清单不是普通文件：{}", path.display())
            } else {
                format!("无法读取模型清单：{}", path.display())
            }));
        }
    };
    let metadata = file.metadata().map_err(|_| {
        ResourceValidationError::new(format!("无法读取模型清单元数据：{}", path.display()))
    })?;
    if !metadata.file_type().is_file() {
        return Err(ResourceValidationError::new(format!(
            "模型清单不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ResourceValidationError::new(format!(
            "模型清单大小 {} 字节超过上限 {MAX_MANIFEST_BYTES}：{}",
            metadata.len(),
            path.display()
        )));
    }
    let mut source = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut source)
        .map_err(|_| {
            ResourceValidationError::new(format!("无法读取模型清单：{}", path.display()))
        })?;
    if source.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ResourceValidationError::new(format!(
            "模型清单实际读取大小 {} 字节超过上限 {MAX_MANIFEST_BYTES}：{}",
            source.len(),
            path.display()
        )));
    }
    let source = String::from_utf8(source).map_err(|_| {
        ResourceValidationError::new(format!("模型清单不是有效 UTF-8：{}", path.display()))
    })?;
    let model = Model3::from_json_str(&source).map_err(|_| {
        ResourceValidationError::new(format!("无法解析模型清单：{}", path.display()))
    })?;
    let resolver = ModelResourceResolver::for_manifest(path).map_err(|error| {
        ResourceValidationError::new(format!("无法建立模型资源解析边界：{error}"))
    })?;

    validate_reference(&resolver, model.moc(), "MOC", MAX_MOC_BYTES)?;

    if model.textures().len() > MAX_TEXTURE_COUNT {
        return Err(ResourceValidationError::new(format!(
            "纹理数量 {} 超过上限 {MAX_TEXTURE_COUNT}",
            model.textures().len()
        )));
    }
    let mut total_texture_pixels = 0_u64;
    for (index, reference) in model.textures().iter().enumerate() {
        let label = format!("纹理 {index}");
        let texture_path =
            validate_reference(&resolver, reference, &label, MAX_TEXTURE_FILE_BYTES)?;
        let (width, height) = image::image_dimensions(&texture_path).map_err(|_| {
            ResourceValidationError::new(format!("无法读取纹理尺寸：{}", texture_path.display()))
        })?;
        if width == 0
            || height == 0
            || width > MAX_TEXTURE_DIMENSION
            || height > MAX_TEXTURE_DIMENSION
        {
            return Err(ResourceValidationError::new(format!(
                "纹理 {} 尺寸 {width}x{height} 超过单边上限 {MAX_TEXTURE_DIMENSION}",
                texture_path.display()
            )));
        }
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
    }

    if let Some(reference) = model.physics() {
        validate_reference(
            &resolver,
            reference,
            "Physics",
            MAX_AUXILIARY_RESOURCE_BYTES,
        )?;
    }
    if let Some(reference) = model.pose() {
        validate_reference(&resolver, reference, "Pose", MAX_AUXILIARY_RESOURCE_BYTES)?;
    }

    Ok(resolver)
}

fn validate_reference(
    resolver: &ModelResourceResolver,
    reference: &str,
    label: &str,
    maximum_bytes: u64,
) -> Result<PathBuf, ResourceValidationError> {
    resolver
        .resolve_file(reference, maximum_bytes)
        .map_err(|error| {
            ResourceValidationError::new(format!("{label} 引用 {reference} 无效：{error}"))
        })
}

/// 描述模型资源预检失败。
#[derive(Debug)]
pub(crate) struct ResourceValidationError {
    message: String,
}

impl ResourceValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ResourceValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ResourceValidationError {}
