use futures::{stream, StreamExt, TryStreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use memmap2::MmapMut;
use rayon::prelude::*;
use tokio_stream::wrappers::ReadDirStream;
use zip::result::ZipResult;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Deserialize, Debug)]
struct TypedValue {
    #[serde(rename = "type", default)]
    pub type_t: String,

    pub value: Option<String>,
    pub values: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct Annotation {
    #[serde(rename = "type")]
    pub type_t: String,

    pub name: String,
    pub target: Option<String>,

    pub value: Option<TypedValue>,
    pub values: Option<HashMap<String, TypedValue>>,
}

#[derive(Deserialize, Debug)]
struct ClassEntry {
    pub name: String,
    pub annotations: Option<Vec<Annotation>>,
    pub interfaces: Option<Vec<String>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().skip(1).next().unwrap_or(OsString::from("."));
    let start = Instant::now();
    let stream = ReadDirStream::new(tokio::fs::read_dir(path)
        .await?);
    let stream = stream
        .try_filter(|file| {
            futures::future::ready(
                file.file_name()
                    .to_str()
                    .map(|x| x.ends_with(".jar"))
                    .unwrap_or(false),
            )
        })
        .try_filter_map(|file| async move {
            //let start = Instant::now();
            let path = &file.path();
            let name = file.file_name().to_string_lossy().to_string();
            let file = File::open(file.path())?;

            let mmap = unsafe { memmap2::Mmap::map(&file) }?;
            let cursor = Cursor::new(mmap);
            let mut archive = zip::ZipArchive::new(cursor).unwrap();
            let mut file = match archive.by_name("META-INF/fml_cache_annotation.json") {
                Ok(f) => f,
                Err(_) => {
                    return Ok(None);
                }
            };
            let mut str = String::with_capacity(file.size() as usize);
            file.read_to_string(&mut str);
            let entries: HashMap<String, ClassEntry> = simd_json::serde::from_str(&mut str)//serde_json::from_str(&str)
                .expect(&format!("JSON error while parsing file {:?}", path));
            //println!("Read {} took {:?}ms", path.to_str().unwrap(), start.elapsed().as_millis());
            Ok(Some((name, entries)))
        })
        .try_filter_map(|(name, map)| async move {
            if map.into_par_iter().flat_map(|(name, entry)| {
                match entry.annotations {
                    None => {
                        vec![].into_par_iter()
                    }
                    Some(x) => {
                        x.into_par_iter()
                    }
                }
            }).any(|annotation| {
                is_client_mod(&annotation)
            }) {
                Ok(Some(name))
            } else {
                Ok(None)
            }
        });

    let mut i = Arc::new(Mutex::new(0u64));

    stream
        .try_for_each_concurrent(32, |x| {
            let mut i = i.clone();
            async move {
                *(i.lock().unwrap()) += 1;
                println!("{}", x);
                Ok(())
            }
        })
        .await?;

    println!(
        "Found {} client-side mods in {}ms",
        i.lock().unwrap(),
        start.elapsed().as_millis()
    );
    Ok(())
}

fn is_client_mod(annotation: &Annotation) -> bool {
    if annotation.name == "Lnet/minecraftforge/fml/common/Mod;" {
        if let Some(values) = &annotation.values {
            return values.iter().any(|(name, value)| {
                name == "clientSideOnly" && value.value.as_ref().map(|x| x == "true").unwrap_or(false)
            });
        }
    }
    false
}