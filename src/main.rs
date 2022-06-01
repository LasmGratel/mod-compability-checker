use futures::{FutureExt, stream, StreamExt, TryFutureExt, TryStreamExt};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use futures::executor::block_on;
use futures::future::Ready;
use memmap2::MmapMut;
use rayon::prelude::*;
use tokio_stream::wrappers::ReadDirStream;
use zip::result::ZipResult;
use serde::{Serialize, Deserialize};

use mimalloc::MiMalloc;
use tokio::fs::DirEntry;
use crate::stream::TryFilter;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Deserialize, Debug)]
struct ModList {
    #[serde(rename = "modListVersion")]
    version: u32,

    #[serde(rename = "modList")]
    list: Vec<ModInfo>,
}

/// mcmod.info, but simplified
#[derive(PartialEq, Debug, Deserialize)]
struct ModInfo {
    modid: String,
    version: String,
    mcversion: Option<String>,
}

#[derive(PartialEq, Debug, Serialize)]
struct Mod {
    id: String,
    version: Option<String>,
    mod_type: ModType,
}

#[derive(PartialEq, Debug, Serialize)]
enum ModType {
    /// Requires on both side
    Normal,

    /// Client only
    ClientOnly,

    /// Don't mind
    AcceptAllRemote
}

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

async fn walk_jar<P: AsRef<Path>>(path: P) -> std::io::Result<TryFilter<ReadDirStream, Ready<bool>, fn(&DirEntry) -> Ready<bool>>> {
    let stream = ReadDirStream::new(tokio::fs::read_dir(path)
        .await?);
    Ok(stream
        .try_filter(|file| {
            futures::future::ready(
                file.file_name()
                    .to_str()
                    .map(|x| x.ends_with(".jar"))
                    .unwrap_or(false),
            )
        }))
}

fn read_archive<P: AsRef<Path>>(path: P) -> Result<(HashMap<String, ClassEntry>, Option<Vec<ModInfo>>), Box<dyn std::error::Error>> {
    let path = path.as_ref();
    let file = File::open(path)?;

    let mmap = unsafe { memmap2::Mmap::map(&file) }?;
    let cursor = Cursor::new(mmap);
    let mut archive = zip::ZipArchive::new(cursor)?;

    let annotations: HashMap<String, ClassEntry> = match archive.by_name("META-INF/fml_cache_annotation.json") {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str);
            simd_json::serde::from_str(&mut str)//serde_json::from_str(&str)
                .expect(&format!("JSON error while parsing file {:?}", path))
        },
        Err(e) => {
            return Err(e.into());
        }
    };

    let info: Option<Vec<ModInfo>> = match archive.by_name("mcmod.info") {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str);
            str = str.replace("\n", "");
            Some(read_mcmod_info(&mut str)
                .expect(&format!("JSON error while parsing mcmod.info from file {:?}", path)))
        },
        Err(e) => {
            return Err(e.into());
        }
    };

    Ok((annotations, info))
}

/// Read mcmod.info string
fn read_mcmod_info(s: &mut str) -> Result<Vec<ModInfo>, simd_json::Error> {
    if let Ok(list) = simd_json::serde::from_str::<Vec<ModInfo>>(s) {
        Ok(list)
    } else {
        Ok(simd_json::serde::from_str::<ModList>(s)?.list)
    }
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
            let file_name = file.file_name().to_string_lossy().to_string();
            if file_name.as_str().to_ascii_lowercase().contains("optifine") {
                return Ok(Some((file_name, None, None, true))); // FUCK OPTIFINE
            }
            read_archive(path)
        })
        .try_filter_map(|(name, map, infos, is_optifine)| async move {
            if is_optifine {
                return Ok(Some(futures::stream::iter(vec![Ok(Mod { id: String::from("OptiFine"), version: None, mod_type: ModType::ClientOnly })].into_iter())));
            }
            let infos = infos.unwrap_or(vec![]).into_iter().map(|x| (x.modid.to_string(), x)).collect::<HashMap<String, ModInfo>>();
            let map = map.unwrap();
            Ok(Some(futures::stream::iter(map.into_par_iter().flat_map(|(name, entry)| {
                match entry.annotations {
                    None => {
                        vec![].into_par_iter()
                    }
                    Some(x) => {
                        x.into_par_iter()
                    }
                }
            }).filter_map(|annotation| {
                if annotation.name == "Lnet/minecraftforge/fml/common/Mod;" {
                    Some(decl_mod_type(&annotation))
                } else {
                    None
                }
            }).filter_map(|o| {
                if let Some((t, modid)) = o {
                    if let Some(info) = infos.get(&modid) {
                        return Some(Ok(Mod {
                            id: modid,
                            mod_type: t,
                            version: Some(info.version.to_string())
                        }));
                    }
                }
                None
            }).collect::<Vec<std::io::Result<Mod>>>().into_iter())))
        }).try_flatten();

    let mut i = Arc::new(Mutex::new(0u64));

    stream
        .try_for_each_concurrent(32, |mod_object| {
            let mut i = i.clone();
            async move {
                *(i.lock().unwrap()) += 1;
                println!("{}", mod_object.id);
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

fn decl_mod_type(annotation: &Annotation) -> Option<(ModType, String)> {
    if let Some(values) = &annotation.values {
        let client_only = values.get("clientSideOnly").map(|x| x.value.as_ref().map(|x| x == "true")).flatten().unwrap_or(false);

        let accept_all_remote = values.get("acceptableRemoteVersions").map(|x| x.value.as_ref().map(|x| x == "*")).flatten().unwrap_or(false);
        let modid = values["modid"].value.as_ref()?.to_string();
        if client_only {
            return Some((ModType::ClientOnly, modid));
        } else if accept_all_remote {
            return Some((ModType::AcceptAllRemote, modid));
        }
    }
    None
}