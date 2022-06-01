extern crate core;

use std::cmp::Ordering;
use futures::{FutureExt, stream, StreamExt, TryFutureExt, TryStreamExt};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::File;

use std::io::{Cursor, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use blake3::Hasher;
use futures::future::Ready;
use rayon::prelude::*;
use tokio_stream::wrappers::ReadDirStream;
use serde::{Serialize, Deserialize};
use clap::Parser;
use memmap2::Mmap;
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

#[derive(Debug, Serialize)]
struct Mod {
    id: String,

    #[serde(skip_serializing)]
    file_name: String,

    version: Option<String>,
    mod_type: ModType,
}

impl PartialEq for Mod {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.version == other.version
    }
}

impl Eq for Mod {}

impl PartialOrd<Self> for Mod {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Mod {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
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

async fn walk_dir<P: AsRef<Path>>(path: P) -> std::io::Result<TryFilter<ReadDirStream, Ready<bool>, fn(&DirEntry) -> Ready<bool>>> {
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

fn read_archive<P: AsRef<Path>>(path: P) -> Result<(String, Option<HashMap<String, ClassEntry>>, Option<Vec<ModInfo>>, bool), Box<dyn std::error::Error>> {
    let path = path.as_ref();
    let file_name = path.file_name().unwrap().to_string_lossy().to_string();
    if file_name.as_str().to_ascii_lowercase().contains("optifine") {
        return Ok((file_name, None, None, true)); // FUCK OPTIFINE
    }
    let file = File::open(path)?;

    let mmap = unsafe { memmap2::Mmap::map(&file) }?;
    let cursor = Cursor::new(mmap);
    let mut archive = zip::ZipArchive::new(cursor)?;

    let annotations: Option<HashMap<String, ClassEntry>> = match archive.by_name("META-INF/fml_cache_annotation.json") {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str)?;
            simd_json::serde::from_str(&mut str)//serde_json::from_str(&str)
                .unwrap_or_else(|_| panic!("JSON error while parsing file {:?}", path))
        },
        Err(_) => {
            None
        }
    };

    let info: Option<Vec<ModInfo>> = match archive.by_name("mcmod.info") {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str)?;
            str = str.replace('\n', "");
            read_mcmod_info(&mut str).ok()
        },
        Err(_) => {
            None
        }
    };

    Ok((file_name, annotations, info, false))
}

fn parse_mods(file_name: String, entries: Option<HashMap<String, ClassEntry>>, mod_infos: Option<Vec<ModInfo>>, is_optifine: bool) -> Vec<Mod> {
    if is_optifine {
        return vec![Mod { id: String::from("OptiFine"), file_name, version: None, mod_type: ModType::ClientOnly }]
    }

    let mut mods = vec![];
    if let Some(entries) = entries {
        if let Some(entries) = parse_entries(entries) {
            let mut mod_map = entries.into_iter().map(|(mod_type, id, version)| {
                (id.clone(), Mod {
                    id,
                    file_name: file_name.clone(),
                    version,
                    mod_type
                })
            }).collect::<HashMap<String, Mod>>();

            // If there is mcmod.info file, match mod id and append versions
            if let Some(infos) = mod_infos {
                // Map consists modid and infos
                infos.into_iter().for_each(|info| {
                    if let Some(x) = mod_map.get_mut(&info.modid) {
                        x.version = Some(info.version);
                    }
                });
            }
            mod_map.into_iter().for_each(|(_, x)| mods.push(x));
        }
    }

    mods
}

// TODO 1.7.10
fn parse_entries(entries: HashMap<String, ClassEntry>) -> Option<Vec<(ModType, String, Option<String>)>> {
    Some(entries
        .into_par_iter()
        .filter_map(|(_, entry)| {
            entry.annotations
        })
        .flatten()
        .filter(|x| x.name == "Lnet/minecraftforge/fml/common/Mod;")
        .filter_map(|x| {
            decl_mod_type(&x)
        })
        .collect()
    )
}

/// Read mcmod.info string
fn read_mcmod_info(s: &mut str) -> Result<Vec<ModInfo>, simd_json::Error> {
    if let Ok(list) = simd_json::serde::from_str::<Vec<ModInfo>>(s) {
        Ok(list)
    } else {
        Ok(simd_json::serde::from_str::<ModList>(s)?.list)
    }
}

#[derive(Parser, Debug)]
#[clap(author, version)]
struct Args {
    /// Mods directory
    #[clap(parse(from_os_str))]
    path: Option<std::path::PathBuf>,

    /// List all the client-side mods or mods that accept all remote versions while parsing.
    #[clap(short, long)]
    verbose: bool,

    /// Hash JAR file instead of modid:version lines
    #[clap(long)]
    strict: bool,

    /// Write a .sha or .strict-sha file in current directory.
    #[clap(short, long)]
    dirty: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Args = Args::parse();
    let path = args.path.unwrap_or_default();

    let stream = walk_dir(&path).await?
        .try_filter_map(|file| async move {
            match read_archive(file.path()) {
                Ok(x) => { Ok(Some(x)) }
                Err(e) => { Err(std::io::Error::new(ErrorKind::Other, e.to_string())) }
            }
        })
        .try_filter_map(|(name, map, infos, is_optifine)| async move {
            Ok(Some(futures::stream::iter(parse_mods(name, map, infos, is_optifine).into_iter().map(Ok).collect::<Vec<std::io::Result<Mod>>>())))
        })
        .try_flatten();

    let (tx, rx) = crossbeam_channel::unbounded();

    stream
        .try_for_each_concurrent(32, |mod_object| {
            let tx = tx.clone();
            async move {
                if mod_object.mod_type != ModType::Normal {
                    if args.verbose {
                        println!("{}", mod_object.file_name);
                    }
                } else {
                    tx.send(mod_object).map_err(|e| std::io::Error::new(ErrorKind::Other, e))?;
                }
                Ok(())
            }
        })
        .await?;

    let mut hash = Hasher::new();
    let x: std::io::Result<()> = rx.try_iter().collect::<BTreeSet<Mod>>().into_iter().try_for_each(|x| {
        if args.strict {
            let file = File::open(path.join(&x.file_name))?;
            let mmap = unsafe { Mmap::map(&file) }?;
            hash.update_rayon(&mmap);
        } else {
            hash.update(format!("{}:{}", &x.id, x.version.unwrap_or_default()).as_bytes());
        }
        Ok(())
    });
    let _ = x?;
    let hash = hash.finalize();
    println!("{}", hash.to_hex());

    if args.dirty {
        if args.strict {
            tokio::fs::write(path.join(".strict-sha"), hash.to_hex().to_string()).await?
        } else {
            tokio::fs::write(path.join(".sha"), hash.to_hex().to_string()).await?
        }
    }

    Ok(())
}

fn decl_mod_type(annotation: &Annotation) -> Option<(ModType, String, Option<String>)> {

    if let Some(values) = &annotation.values {
        let client_only = values.get("clientSideOnly").and_then(|x| x.value.as_ref().map(|x| x == "true")).unwrap_or(false);

        let accept_all_remote = values.get("acceptableRemoteVersions").and_then(|x| x.value.as_ref().map(|x| x == "*")).unwrap_or(false);
        let version = values.get("version").and_then(|x| x.value.clone());
        let modid = values["modid"].value.as_ref()?.to_string();
        return if client_only {
            Some((ModType::ClientOnly, modid, version))
        } else if accept_all_remote {
            Some((ModType::AcceptAllRemote, modid, version))
        } else {
            Some((ModType::Normal, modid, version))
        }
    }
    None
}