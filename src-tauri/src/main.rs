#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{Emitter, Manager};
use waw_asset_manager::{
    conversion,
    formats::{Entry, Schema},
    images,
    storage::{self, Document, SourceFile},
};

#[derive(Clone)]
struct Context {
    root: PathBuf,
    cache: PathBuf,
    schemas: Vec<Schema>,
    _instance_lock: Arc<std::fs::File>,
}

#[derive(Default)]
struct Runtime {
    context: Mutex<Option<Context>>,
    busy: AtomicBool,
    cancel: Arc<AtomicBool>,
}

impl Runtime {
    fn context(&self) -> Result<Context, String> {
        self.context
            .lock()
            .map_err(|error| error.to_string())?
            .clone()
            .ok_or("Open this app beside CoDWaW.exe first.".into())
    }
    fn available(&self) -> Result<Context, String> {
        if self.busy.load(Ordering::SeqCst) {
            return Err("Wait for the current conversion to finish.".into());
        }
        self.context()
    }
}

fn game_root() -> Result<PathBuf, String> {
    let root = if cfg!(debug_assertions) {
        std::env::var_os("WAW_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.local/test-root")
            })
    } else {
        std::env::current_exe()
            .map_err(|error| error.to_string())?
            .parent()
            .ok_or("Cannot locate the app folder")?
            .to_path_buf()
    };
    if !root.join("CoDWaW.exe").is_file() {
        return Err("Put this executable in your World at War folder, beside CoDWaW.exe, then open it again.".into());
    }
    if !root.join("bin/converter.exe").is_file() || !root.join("deffiles/material.gdf").is_file() {
        return Err("Install the World at War Modtools in this game folder first. The original bin/converter.exe and deffiles are required.".into());
    }
    let canonical = root.canonicalize().map_err(|error| error.to_string())?;
    let text = canonical.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return Ok(PathBuf::from(format!(r"\\{unc}")));
    }
    Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)))
}

#[derive(serde::Serialize)]
struct Startup {
    root: String,
    cache: String,
    schemas: Vec<Schema>,
    files: Vec<SourceFile>,
}

#[tauri::command]
async fn startup(
    app: tauri::AppHandle,
    state: tauri::State<'_, Runtime>,
) -> Result<Startup, String> {
    let root = game_root()?;
    let cache = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    let context = tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&cache).map_err(|error| error.to_string())?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(cache.join(format!(
                "{}.lock",
                storage::hash(root.to_string_lossy().as_bytes())
            )))
            .map_err(|error| error.to_string())?;
        lock.try_lock().map_err(|_| {
            "This Asset Manager is already open for this game folder. Close the other copy first."
        })?;
        let schemas = storage::schemas(&root)?;
        let files = storage::list_gdts(&root)?;
        Ok::<_, String>((
            Context {
                root,
                cache,
                schemas,
                _instance_lock: Arc::new(lock),
            },
            files,
        ))
    })
    .await
    .map_err(|error| error.to_string())??;
    let response = Startup {
        root: context.0.root.display().to_string(),
        cache: context.0.cache.display().to_string(),
        schemas: context.0.schemas.clone(),
        files: context.1,
    };
    *state.context.lock().map_err(|error| error.to_string())? = Some(context.0);
    Ok(response)
}

#[tauri::command]
async fn list_files(state: tauri::State<'_, Runtime>) -> Result<Vec<SourceFile>, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || storage::list_gdts(&context.root))
        .await
        .map_err(|error| error.to_string())?
}

#[derive(serde::Serialize)]
struct ReferenceEntry {
    name: String,
    schema: String,
}

#[tauri::command]
async fn reference_entries(
    state: tauri::State<'_, Runtime>,
) -> Result<Vec<ReferenceEntry>, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut entries = Vec::new();
        for source in storage::list_gdts(&context.root)? {
            for entry in waw_asset_manager::formats::parse_gdt(&storage::read_text(
                &context.root.join(source.path),
            )?)? {
                entries.push(ReferenceEntry {
                    name: entry.name,
                    schema: entry.schema,
                });
            }
        }
        Ok(entries)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn open_gdt(
    path: Option<String>,
    state: tauri::State<'_, Runtime>,
) -> Result<Option<Document>, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || {
        let path = match path {
            Some(path) => path,
            None => match rfd::FileDialog::new()
                .set_directory(context.root.join("source_data"))
                .add_filter("Game data table", &["gdt"])
                .pick_file()
            {
                Some(path) => storage::relative(&context.root, &path)?,
                None => return Ok(None),
            },
        };
        storage::open(&context.root, &path).map(Some)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn save_gdt(
    document: Document,
    state: tauri::State<'_, Runtime>,
) -> Result<Document, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || storage::save(&context.root, &document))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn pick_property(
    directory: String,
    multiple: bool,
    image: bool,
    state: tauri::State<'_, Runtime>,
) -> Result<Option<String>, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || {
        let start_directory = if image && directory.is_empty() {
            "texture_assets"
        } else {
            &directory
        };
        let start = storage::inside(&context.root, start_directory)?;
        let mut dialog = rfd::FileDialog::new().set_directory(start);
        if image {
            dialog = dialog.add_filter("Images", &["png", "tga", "dds", "jpg", "jpeg"]);
        }
        let paths = if multiple {
            dialog.pick_files()
        } else {
            dialog.pick_file().map(|path| vec![path])
        };
        paths
            .map(|paths| {
                paths
                    .iter()
                    .map(|path| {
                        storage::relative(&context.root, path).map(|path| {
                            // GDF reldir controls store paths relative to that directory.
                            path.strip_prefix(&format!("{}/", directory.trim_end_matches('/')))
                                .unwrap_or(&path)
                                .to_owned()
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|values| values.join(";"))
            })
            .transpose()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn prepare_property_image(
    path: String,
    state: tauri::State<'_, Runtime>,
) -> Result<String, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || images::prepare_property(&context.root, &path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn import_images(
    paths: Option<Vec<String>>,
    state: tauri::State<'_, Runtime>,
) -> Result<Vec<images::Input>, String> {
    let context = state.available()?;
    tauri::async_runtime::spawn_blocking(move || {
        let paths = paths.unwrap_or_else(|| {
            rfd::FileDialog::new()
                .set_directory(context.root.join("texture_assets"))
                .add_filter("Images", &["png", "tga"])
                .pick_files()
                .unwrap_or_default()
                .iter()
                .map(|path| path.to_string_lossy().into())
                .collect()
        });
        paths
            .iter()
            .map(|path| images::inspect(std::path::Path::new(path)))
            .collect()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn prepare_images(
    request: images::Preparation,
    state: tauri::State<'_, Runtime>,
) -> Result<Vec<Entry>, String> {
    let context = state.available()?;
    let material = context
        .schemas
        .iter()
        .find(|schema| schema.file == "material.gdf")
        .ok_or("Material schema missing")?;
    if !material
        .fields
        .iter()
        .find(|field| field.name == "materialType")
        .is_some_and(|field| field.choices.contains(&request.material_type))
    {
        return Err("Choose an installed material type.".into());
    }
    tauri::async_runtime::spawn_blocking(move || images::prepare(&context.root, request))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn settings(
    value: Option<serde_json::Value>,
    state: tauri::State<'_, Runtime>,
) -> Result<serde_json::Value, String> {
    let context = state.available()?;
    let path = context.cache.join("image-rules.json");
    if let Some(value) = value {
        storage::atomic_write(
            &path,
            &serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
        )?;
        return Ok(value);
    }
    if path.exists() {
        serde_json::from_str(&storage::read_text(&path)?).map_err(|error| error.to_string())
    } else {
        Ok(serde_json::Value::Null)
    }
}

#[tauri::command]
async fn convert(
    document: Document,
    ids: Vec<usize>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Runtime>,
) -> Result<String, String> {
    let context = state.context()?;
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("Conversion is already running.".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    let cancel = state.cancel.clone();
    let selected: Vec<Entry> = document
        .entries
        .iter()
        .filter(|entry| ids.contains(&entry.id))
        .cloned()
        .collect();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let on_disk = storage::open(&context.root, &document.path)?;
        if on_disk.fingerprint != document.fingerprint {
            return Err("The GDT changed since saving. Reopen it before converting.".into());
        }
        let lines = Arc::new(Mutex::new(String::new()));
        let saved = lines.clone();
        let log = Arc::new(move |line: String| {
            if let Ok(mut text) = saved.lock() {
                text.push_str(&line);
                text.push('\n');
            }
            let _ = app.emit("conversion-log", line);
        });
        let result = conversion::run(&context.root, &context.schemas, &selected, cancel, log);
        if let Ok(text) = lines.lock() {
            let _ =
                storage::atomic_write(&context.cache.join("last-conversion.txt"), text.as_bytes());
        }
        result
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    state.busy.store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
fn cancel(state: tauri::State<Runtime>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() {
    let profile = tempfile::Builder::new()
        .prefix("waw-assets-ui-")
        .tempdir()
        .expect("Cannot create the temporary browser profile");
    let profile_path = profile.path().to_path_buf();
    tauri::Builder::default()
        .manage(Runtime::default())
        .setup(move |app| {
            tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .data_directory(profile_path)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup,
            list_files,
            reference_entries,
            open_gdt,
            save_gdt,
            pick_property,
            prepare_property_image,
            import_images,
            prepare_images,
            settings,
            convert,
            cancel,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("Cannot start WaW Asset Manager");
    // Keep the private WebView profile alive until the app exits. This also
    // avoids blank windows from old per-folder WebView profiles in game roots.
    drop(profile);
}
