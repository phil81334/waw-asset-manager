//! Invoke the user's converter. Never recreate or redistribute Treyarch's compiler.
use crate::{
    formats::{self, Entry, Schema},
    storage,
};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

#[cfg(windows)]
fn hidden(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hidden(_command: &mut Command) {}

// A job object kills only this app's converter and its children if the app exits
// unexpectedly. It never targets a game, editor or another user's converter.
#[cfg(windows)]
mod owned_job {
    use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Child};
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            data: *const c_void,
            size: u32,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn TerminateJobObject(handle: *mut c_void, code: u32) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        process_time: i64,
        job_time: i64,
        flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    pub struct Job(*mut c_void);
    impl Job {
        pub fn attach(child: &Child) -> Result<Self, String> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let job = Self(handle);
                let mut limits = ExtendedLimits::default();
                limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                if SetInformationJobObject(
                    handle,
                    9,
                    &limits as *const _ as *const c_void,
                    std::mem::size_of::<ExtendedLimits>() as u32,
                ) == 0
                    || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
                {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                Ok(job)
            }
        }
        pub fn stop(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }
    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub fn converter_running() -> Result<bool, String> {
    let mut command = Command::new("tasklist.exe");
    command.args(["/FI", "IMAGENAME eq converter.exe", "/FO", "CSV", "/NH"]);
    hidden(&mut command);
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("Could not check for another running converter.".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .to_lowercase()
        .contains("\"converter.exe\""))
}

fn verify_outputs(root: &Path, entry: &Entry) -> Result<(), String> {
    let database = Connection::open_with_flags(
        root.join("assettgtcache/converter.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| error.to_string())?;
    // The cache is only read. Treyarch remains its sole writer.
    let mut query = database.prepare(
        "select distinct f.path from file_refs r join file_ids f using(file_id) join cvt_ids v using(cvt_id) \
         join gdt_ids g using(gdt_id) where g.name=?1 and g.gdf=?2 and r.type=2"
    ).map_err(|error| error.to_string())?;
    let paths = query
        .query_map([&entry.name, &entry.schema], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if paths.is_empty() {
        return Err(format!(
            "{}: native converter recorded no output files.",
            entry.name
        ));
    }
    for relative in paths {
        let path = storage::inside(root, &relative)?;
        if !path.is_file() || path.metadata().map_err(|error| error.to_string())?.len() == 0 {
            return Err(format!("Missing or empty native output: {relative}"));
        }
    }
    Ok(())
}

fn image_names(entry: &Entry) -> Vec<(String, String)> {
    if entry.schema != "material.gdf" {
        return Vec::new();
    }
    entry
        .properties
        .iter()
        .filter_map(|(field, value)| {
            if !field.contains("Map") {
                return None;
            }
            let source = value.replace('\\', "/").to_lowercase();
            let path = Path::new(&source);
            let extension = path.extension()?.to_str()?;
            if !["tga", "dds", "jpg", "jpeg"].contains(&extension) {
                return None;
            }
            Some((path.file_stem()?.to_string_lossy().into_owned(), source))
        })
        .collect()
}

pub fn run(
    root: &Path,
    schemas: &[Schema],
    selected: &[Entry],
    cancel: Arc<AtomicBool>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<String, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("Conversion cancelled.".into());
    }
    if selected.is_empty() {
        return Err("Select at least one saved asset.".into());
    }
    for entry in selected {
        let schema = schemas
            .iter()
            .find(|schema| schema.file == entry.schema)
            .ok_or("Asset schema is not installed")?;
        if schema.plugins.is_empty()
            || schema
                .plugins
                .iter()
                .any(|plugin| !root.join(format!("deffiles/{plugin}.dll")).is_file())
        {
            return Err(format!(
                "{} has no complete installed PC converter plugin.",
                schema.name
            ));
        }
        if !formats::valid_name(&entry.name) {
            return Err(format!(
                "Invalid asset name for native conversion: {}",
                entry.name
            ));
        }
    }
    if converter_running()? {
        return Err("Another converter is running. Wait for it to finish first.".into());
    }
    let mut owners: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut image_sources: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for file in storage::list_gdts(root)? {
        if cancel.load(Ordering::SeqCst) {
            return Err("Conversion cancelled.".into());
        }
        for entry in formats::parse_gdt(&storage::read_text(&root.join(&file.path))?)? {
            for (name, source) in image_names(&entry) {
                image_sources.entry(name).or_default().insert(source);
            }
            owners
                .entry((entry.schema.to_lowercase(), entry.name.to_lowercase()))
                .or_default()
                .push(file.path.clone());
        }
    }
    for entry in selected {
        for (name, _) in image_names(entry) {
            if image_sources
                .get(&name)
                .is_some_and(|sources| sources.len() > 1)
            {
                return Err(format!("Image output name '{name}.iwi' is used by different source paths. Give these inputs unique filenames before converting."));
            }
        }
        let paths = owners
            .get(&(entry.schema.to_lowercase(), entry.name.to_lowercase()))
            .ok_or("Save the asset before converting.")?;
        if paths.len() != 1 {
            return Err(format!(
                "Duplicate {} in {}. Rename or remove the duplicate before conversion.",
                entry.name,
                paths.join(", ")
            ));
        }
    }
    let result = (|| {
        for (index, entry) in selected.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err("Conversion cancelled.".into());
            }
            let kind = entry.schema.trim_end_matches(".gdf");
            let owner = &owners[&(entry.schema.to_lowercase(), entry.name.to_lowercase())][0];
            let source = storage::inside(root, owner)?;
            let current = formats::parse_gdt(&storage::read_text(&source)?)?;
            if !current.iter().any(|value| {
                value.name == entry.name
                    && value.schema == entry.schema
                    && value.properties == entry.properties
            }) {
                return Err(
                    "A selected asset changed outside the app; reopen its GDT before converting."
                        .into(),
                );
            }
            // Refresh the actual source timestamp, not just the launch time.
            // This also defeats a same-second edit made by an outside editor.
            let modified = source
                .metadata()
                .map_err(|error| error.to_string())?
                .modified()
                .map_err(|error| error.to_string())?;
            let next = std::time::UNIX_EPOCH
                + Duration::from_secs(
                    modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_secs()
                        + 1,
                );
            if let Ok(delay) = next.duration_since(std::time::SystemTime::now()) {
                if delay > Duration::from_secs(2) {
                    return Err(
                        "Selected GDT has a future timestamp; correct its date first.".into(),
                    );
                }
                let deadline = std::time::Instant::now() + delay + Duration::from_millis(20);
                while std::time::Instant::now() < deadline {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("Conversion cancelled.".into());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
            fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .map_err(|error| error.to_string())?
                .set_modified(std::time::SystemTime::now())
                .map_err(|error| error.to_string())?;
            log(format!(
                "[{}/{}] {kind}: {}",
                index + 1,
                selected.len(),
                entry.name
            ));
            if cancel.load(Ordering::SeqCst) {
                return Err("Conversion cancelled.".into());
            }
            let mut command = Command::new(root.join("bin/converter.exe"));
            command
                .args(["-nopause", "-nocachedownload", "-single", kind, &entry.name])
                .current_dir(root.join("bin"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            hidden(&mut command);
            let mut child = command
                .spawn()
                .map_err(|error| format!("Could not start converter.exe: {error}"))?;
            #[cfg(windows)]
            let job = match owned_job::Job::attach(&child) {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Could not protect converter process: {error}"));
                }
            };
            let streams: Vec<Box<dyn std::io::Read + Send>> = vec![
                Box::new(child.stdout.take().unwrap()),
                Box::new(child.stderr.take().unwrap()),
            ];
            let readers: Vec<_> = streams
                .into_iter()
                .map(|stream| {
                    let log = log.clone();
                    thread::spawn(move || {
                        for line in BufReader::new(stream).split(b'\n') {
                            match line {
                                Ok(bytes) => log(String::from_utf8_lossy(&bytes).trim_end().into()),
                                Err(error) => {
                                    log(format!("Could not read converter log: {error}"));
                                    break;
                                }
                            }
                        }
                    })
                })
                .collect();
            let status = loop {
                if cancel.load(Ordering::SeqCst) {
                    #[cfg(windows)]
                    job.stop();
                    let _ = child.kill();
                }
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    break status;
                }
                thread::sleep(Duration::from_millis(50));
            };
            #[cfg(windows)]
            drop(job);
            for reader in readers {
                let _ = reader.join();
            }
            if cancel.load(Ordering::SeqCst) {
                return Err("Conversion cancelled.".into());
            }
            if !status.success() {
                return Err(format!(
                    "{} failed (exit {}). See the native log.",
                    entry.name,
                    status.code().unwrap_or(-1)
                ));
            }
            verify_outputs(root, entry)?;
        }
        Ok(format!("Converted {} asset(s).", selected.len()))
    })();
    result
}
