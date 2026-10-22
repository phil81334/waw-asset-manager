use crate::{
    formats::{Entry, Properties},
    storage,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{
    codecs::{png::PngEncoder, tga::TgaEncoder},
    ExtendedColorType, ImageEncoder, RgbaImage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

pub const ROLES: [&str; 5] = [
    "colorMap",
    "normalMap",
    "detailMap",
    "specColorMap",
    "cosinePowerMap",
];

fn load(path: &Path) -> Result<RgbaImage, String> {
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if extension != "png" && extension != "tga" {
        return Err("Image import accepts PNG and TGA files.".into());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if extension == "png" && bytes.get(24) == Some(&16) {
        return Err(format!(
            "{}: 16-bit PNG is not supported in this version. Save an 8-bit copy first.",
            path.display()
        ));
    }
    let format = if extension == "png" {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Tga
    };
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let image = reader
        .decode()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(image.to_rgba8())
}

#[derive(Serialize)]
pub struct Input {
    pub path: String,
    pub filename: String,
    pub width: u32,
    pub height: u32,
    pub thumbnail: String,
}

pub fn inspect(path: &Path) -> Result<Input, String> {
    let pixels = load(path)?;
    let thumb = image::imageops::thumbnail(&pixels, 96, 72);
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            &thumb,
            thumb.width(),
            thumb.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    Ok(Input {
        path: path.to_string_lossy().into(),
        filename: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        width: pixels.width(),
        height: pixels.height(),
        thumbnail: format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
    })
}

/// Prepare one property image without resizing or changing the source. A
/// separate name protects an existing TGA, including files made by other tools.
pub fn prepare_property(root: &Path, source: &str) -> Result<String, String> {
    let source_path = Path::new(source);
    let relative = if source_path.is_absolute() {
        storage::relative(root, source_path)?
    } else {
        source.replace('\\', "/")
    };
    let path = storage::inside(root, &relative)?;
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        return Ok(source.to_owned());
    }
    let pixels = load(&path)?;
    let mut bytes = Vec::new();
    TgaEncoder::new(&mut bytes)
        .disable_rle()
        .encode(
            &pixels,
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    let original_stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let stem: String = original_stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(23)
        .collect();
    // IWI output names are shared across folders. Include the source identity
    // so two different PNGs called e.g. brick.png cannot share that output name.
    let mut identity = storage::relative(root, &path)?.to_lowercase().into_bytes();
    identity.push(0);
    identity.extend_from_slice(&bytes);
    let digest = storage::hash(&identity);
    let parent = path.parent().ok_or("Cannot locate the image folder.")?;
    for attempt in 0..1000 {
        // The native converter limits image names to 42 characters including
        // .iwi. Keep even the numbered conflict names below that limit.
        let filename = if attempt == 0 {
            format!("{stem}_{}.tga", &digest[..10])
        } else {
            format!("{stem}_{}_{attempt}.tga", &digest[..10])
        };
        let target = parent.join(filename);
        let relative = target
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let target = storage::inside(root, &relative)?;
        if target.exists() {
            if target.is_file() && fs::read(&target).map_err(|error| error.to_string())? == bytes {
                return Ok(relative);
            }
            continue;
        }
        // Publish only a complete file. Never replace a file that appeared
        // after the existence check (for example, another app's output).
        let mut pending =
            tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
        pending
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        pending
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        match pending.persist_noclobber(&target) {
            Ok(_) => return Ok(relative),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.to_string()),
        }
    }
    Err("Could not choose an unused TGA filename beside this PNG.".into())
}

#[derive(Deserialize)]
pub struct Assignment {
    pub path: String,
    pub name: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct Preparation {
    pub project: String,
    pub material_type: String,
    pub assignments: Vec<Assignment>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Resizing is explicit. Premultiplication avoids dark fringes around transparent
/// colour images; normal maps are filtered as vectors and normalised afterwards.
pub fn resized(pixels: &RgbaImage, width: u32, height: u32, normal: bool) -> RgbaImage {
    let mut floats = image::Rgba32FImage::new(pixels.width(), pixels.height());
    for (source, target) in pixels.pixels().zip(floats.pixels_mut()) {
        let alpha = f32::from(source[3]) / 255.0;
        for channel in 0..3 {
            let value = f32::from(source[channel]) / 255.0;
            target[channel] = if normal {
                value * 2.0 - 1.0
            } else {
                value * alpha
            };
        }
        target[3] = alpha;
    }
    let filtered = image::imageops::resize(
        &floats,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    let mut result = RgbaImage::new(width, height);
    for (source, target) in filtered.pixels().zip(result.pixels_mut()) {
        let length = (source[0].powi(2) + source[1].powi(2) + source[2].powi(2)).sqrt();
        for channel in 0..3 {
            let value = if normal {
                let vector = if length > 0.00001 {
                    source[channel] / length
                } else if channel == 2 {
                    1.0
                } else {
                    0.0
                };
                vector * 0.5 + 0.5
            } else if source[3] > 0.00001 {
                source[channel] / source[3]
            } else {
                0.0
            };
            target[channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        target[3] = (source[3].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    result
}

pub fn prepare(root: &Path, request: Preparation) -> Result<Vec<Entry>, String> {
    let flat_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 100
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    if !flat_name(&request.project) || request.assignments.is_empty() {
        return Err(
            "Choose a project name using letters, numbers and underscores, and add images.".into(),
        );
    }
    if request.width.is_some() != request.height.is_some() {
        return Err("Choose both target dimensions, or keep both unchanged.".into());
    }
    let mut keys = HashSet::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut outputs: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for assignment in request.assignments {
        if !flat_name(&assignment.name) || !ROLES.contains(&assignment.role.as_str()) {
            return Err("Every image needs a valid material name and texture role.".into());
        }
        if !keys.insert((assignment.name.to_lowercase(), assignment.role.clone())) {
            return Err(format!(
                "More than one {} for {}",
                assignment.role, assignment.name
            ));
        }
        let mut pixels = load(Path::new(&assignment.path))?;
        if let (Some(width), Some(height)) = (request.width, request.height) {
            if width == 0 || height == 0 || width > pixels.width() || height > pixels.height() {
                return Err(format!(
                    "{} is too small for {width} x {height}; upscaling is disabled.",
                    assignment.path
                ));
            }
            if (width, height) != pixels.dimensions() {
                pixels = resized(
                    &pixels,
                    width,
                    height,
                    matches!(assignment.role.as_str(), "normalMap" | "detailMap"),
                );
            }
        }
        let mut tga = Vec::new();
        TgaEncoder::new(&mut tga)
            .disable_rle()
            .encode(
                &pixels,
                pixels.width(),
                pixels.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(|error| error.to_string())?;
        let mut identity = request.project.as_bytes().to_vec();
        identity.extend_from_slice(assignment.name.as_bytes());
        identity.extend_from_slice(assignment.role.as_bytes());
        identity.extend_from_slice(&tga);
        let digest = storage::hash(&identity);
        let role = match assignment.role.as_str() {
            "colorMap" => "c",
            "normalMap" => "n",
            "detailMap" => "d",
            "specColorMap" => "s",
            _ => "g",
        };
        // Native IWI output filenames are limited to 42 characters, including
        // the extension. Shorten only our copy's name; preserve the source name.
        let short_name = &assignment.name[..assignment.name.len().min(24)];
        let filename = format!("{}_{}_{}.tga", short_name, role, &digest[..10]);
        let relative = format!("texture_assets/{}/{}", request.project, filename);
        let target = storage::inside(root, &relative)?;
        if target.exists() && fs::read(&target).map_err(|error| error.to_string())? != tga {
            return Err(format!("An existing prepared image differs: {relative}"));
        }
        let index = entries
            .iter()
            .position(|entry| entry.name == assignment.name)
            .unwrap_or_else(|| {
                let mut properties = Properties::new();
                properties.insert("materialType".into(), request.material_type.clone());
                properties.insert("surfaceType".into(), "<none>".into());
                entries.push(Entry {
                    id: entries.len(),
                    name: assignment.name.clone(),
                    schema: "material.gdf".into(),
                    properties,
                });
                entries.len() - 1
            });
        entries[index].properties.insert(assignment.role, relative);
        outputs.push((target, tga));
    }
    if entries
        .iter()
        .any(|entry| !entry.properties.contains_key("colorMap"))
    {
        return Err(
            "Each material needs one colour image. Correct the assignments before preparing."
                .into(),
        );
    }
    // Validate the complete batch before creating anything. Original inputs and
    // existing prepared files are never overwritten, even on a partial failure.
    let mut created = Vec::new();
    let result = (|| {
        for (path, bytes) in outputs {
            if path.exists() {
                if fs::read(&path).map_err(|error| error.to_string())? != bytes {
                    return Err(
                        "A prepared image changed outside the app. Nothing was overwritten.".into(),
                    );
                }
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).map_err(|error| error.to_string())?;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            created.push(path);
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
        Ok(entries)
    })();
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
    }
    result
}
