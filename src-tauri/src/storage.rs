use crate::formats::{self, Entry, Schema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Never allow generated paths to escape through '..', drive prefixes or junctions.
pub fn inside(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err("Use a path inside the World at War folder.".into());
    }
    let target = root.join(relative_path);
    let mut existing = target.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or("Invalid target path")?;
    }
    let resolved = existing.canonicalize().map_err(|error| error.to_string())?;
    if !resolved.starts_with(root.canonicalize().map_err(|error| error.to_string())?) {
        return Err("The target resolves outside the World at War folder.".into());
    }
    Ok(target)
}

pub fn relative(root: &Path, path: &Path) -> Result<String, String> {
    let resolved = path.canonicalize().map_err(|error| error.to_string())?;
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    Ok(resolved
        .strip_prefix(root)
        .map_err(|_| "Choose a file inside World at War.")?
        .to_string_lossy()
        .replace('\\', "/"))
}

pub fn schemas(root: &Path) -> Result<Vec<Schema>, String> {
    let list = read_text(&root.join("bin/converter_gdf_files_0.txt"))?;
    let mut result = Vec::new();
    for filename in list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "input.gdf")
    {
        let path = inside(root, &format!("deffiles/{filename}"))?;
        result.push(
            formats::parse_schema(filename, &read_text(&path)?)
                .map_err(|error| format!("{filename}: {error}"))?,
        );
    }
    Ok(result)
}

pub fn source_folders(root: &Path) -> Result<Vec<String>, String> {
    Ok(read_text(&root.join("bin/converter_gdt_dirs_0.txt"))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn walk_files(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if !folder.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(folder).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        // Do not follow directory links into another tree, or recurse in a loop.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            files.extend(walk_files(&entry.path())?);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Clone, Serialize)]
pub struct SourceFile {
    pub path: String,
    pub fingerprint: String,
}

pub fn list_gdts(root: &Path) -> Result<Vec<SourceFile>, String> {
    let mut result = Vec::new();
    for directory in source_folders(root)? {
        for path in walk_files(&inside(root, &directory)?)? {
            if path
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case("gdt"))
            {
                let bytes = fs::read(&path).map_err(|error| error.to_string())?;
                result.push(SourceFile {
                    path: relative(root, &path)?,
                    fingerprint: hash(&bytes),
                });
            }
        }
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    result.dedup_by(|left, right| left.path == right.path);
    Ok(result)
}

fn gdt_target(root: &Path, path: &str) -> Result<PathBuf, String> {
    let target = inside(root, path)?;
    if !target
        .extension()
        .is_some_and(|value| value.eq_ignore_ascii_case("gdt"))
    {
        return Err("Choose a .gdt file.".into());
    }
    let allowed = source_folders(root)?
        .iter()
        .any(|folder| Path::new(path).starts_with(folder));
    if !allowed {
        return Err("Save GDTs in a configured source folder, such as source_data.".into());
    }
    Ok(target)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Document {
    pub path: String,
    pub fingerprint: String,
    pub entries: Vec<Entry>,
}

pub fn open(root: &Path, path: &str) -> Result<Document, String> {
    let text = read_text(&gdt_target(root, path)?)?;
    Ok(Document {
        path: path.into(),
        fingerprint: hash(text.as_bytes()),
        entries: formats::parse_gdt(&text)?,
    })
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("No parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    use std::io::Write;
    temporary
        .write_all(bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

pub fn save(root: &Path, document: &Document) -> Result<Document, String> {
    let target = gdt_target(root, &document.path)?;
    let original = if target.exists() {
        read_text(&target)?
    } else {
        "{\n}\n".into()
    };
    if (target.exists() && hash(original.as_bytes()) != document.fingerprint)
        || (!target.exists() && !document.fingerprint.is_empty())
    {
        return Err("This GDT changed outside the app. Reopen it before saving; your edits have not been written.".into());
    }
    let mut keys = std::collections::HashSet::new();
    for entry in &document.entries {
        let key = (entry.name.to_lowercase(), entry.schema.to_lowercase());
        if !keys.insert(key) {
            return Err(format!("Duplicate asset in this GDT: {}", entry.name));
        }
    }
    let text = formats::rewrite_gdt(&original, &document.entries)?;
    if text == original && target.exists() {
        return open(root, &document.path);
    }
    if target.exists() {
        // Native cache timestamps have whole-second precision. Wait BEFORE the
        // atomic replacement, so the saved file itself has a distinct timestamp.
        let modified = target
            .metadata()
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        let next = UNIX_EPOCH + Duration::from_secs(modified + 1) + Duration::from_millis(20);
        if let Ok(delay) = next.duration_since(SystemTime::now()) {
            if delay > Duration::from_secs(2) {
                return Err(
                    "The GDT has a future timestamp; correct its date before saving.".into(),
                );
            }
            thread::sleep(delay);
        }
        if hash(read_text(&target)?.as_bytes()) != document.fingerprint {
            return Err("The GDT changed while saving. Nothing was overwritten.".into());
        }
    }
    atomic_write(&target, text.as_bytes())?;
    open(root, &document.path)
}
