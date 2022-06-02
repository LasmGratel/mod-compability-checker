extern crate core;

use std::cmp::Ordering;
use futures::{stream, TryStreamExt};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;

use std::io::{Cursor, ErrorKind, Read};
use std::path::{Path};
use std::vec::IntoIter;
use blake3::Hasher;
use futures::future::Ready;
use rayon::prelude::*;
use tokio_stream::wrappers::ReadDirStream;
use serde::{Serialize, Deserialize};
use clap::Parser;
use memmap2::{Mmap};
use mimalloc::MiMalloc;
use tokio::fs::DirEntry;
use crate::stream::TryFilter;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Deserialize, Debug)]
struct ModList {
    //#[serde(rename = "modListVersion")]
    //version: u32,

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
    //#[serde(rename = "type", default)]
    //pub type_t: String,

    pub value: Option<String>,
    //pub values: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct Annotation {
    //#[serde(rename = "type")]
    //pub type_t: String,

    pub name: String,
    //pub target: Option<String>,

    //pub value: Option<TypedValue>,
    pub values: Option<HashMap<String, TypedValue>>,
}

#[derive(Deserialize, Debug)]
struct ClassEntry {
    //pub name: String,
    pub annotations: Option<Vec<Annotation>>,
    //pub interfaces: Option<Vec<String>>,
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

type ArchiveInfo = (String, Option<HashMap<String, ClassEntry>>, Option<Vec<ModInfo>>, bool);

async fn read_archive<P: AsRef<Path>>(path: P) -> std::io::Result<Option<ArchiveInfo>> {
    let path = path.as_ref();
    let file_name = path.file_name().unwrap().to_string_lossy().to_string();
    if file_name.as_str().to_ascii_lowercase().contains("optifine") {
        return Ok(Some((file_name, None, None, true))); // FUCK OPTIFINE
    }
    let file = File::open(path)?;

    let mmap = unsafe { memmap2::Mmap::map(&file) }?;

    let cursor = Cursor::new(&mmap);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut a2 = archive.clone();

    let annotations = archive.by_name("META-INF/fml_cache_annotation.json");
    let annotations = async { match annotations {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str).unwrap();
            Some(simd_json::serde::from_str::<HashMap<String, ClassEntry>>(&mut str)//serde_json::from_str(&str)
                .unwrap_or_else(|_| panic!("JSON error while parsing file {:?}", path)))
        },
        Err(_) => {
            None
        }
    } };

    let info = a2.by_name("mcmod.info");
    let info = async { match info {
        Ok(mut f) => {
            let mut str = String::with_capacity(f.size() as usize);
            f.read_to_string(&mut str).unwrap();
            str = str.replace('\n', "");
            read_mcmod_info(&mut str).ok()
        },
        Err(_) => {
            None
        }
    } };

    Ok(Some((file_name, annotations.await, info.await, false)))
}

async fn parse_mods(file_name: String, entries: Option<HashMap<String, ClassEntry>>, mod_infos: Option<Vec<ModInfo>>, is_optifine: bool) -> std::io::Result<Option<futures::stream::Iter<IntoIter<std::io::Result<Mod>>>>> {
    if is_optifine {
        return Ok(Some(futures::stream::iter(vec![Ok(Mod { id: String::from("OptiFine"), file_name, version: None, mod_type: ModType::ClientOnly })].into_iter())))
    }

    let mut mods = vec![];
    if let Some(entries) = entries {
        let mut mod_map = entries
            .into_par_iter()
            .filter_map(|(_, entry)| {
                entry.annotations
            })
            .flatten()
            .filter(|x| x.name == "Lnet/minecraftforge/fml/common/Mod;")
            .filter_map(|x| {
                decl_mod_type(&x)
            })
            .map(|(mod_type, id, version)| {
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
        mod_map.into_iter().for_each(|(_, x)| mods.push(Ok(x)));
    }

    Ok(Some(futures::stream::iter(mods.into_iter())))
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
struct Cli {
    /// Threads used to read mod files, default to CPU num.
    #[clap(short, long, default_value_t = num_cpus::get())]
    threads: usize,

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
    let args: Cli = Cli::parse();
    let path = args.path.unwrap_or_default();

    let stream = walk_dir(&path).await?
        .try_filter_map(|file| read_archive(file.path()))
        .try_filter_map(|(name, map, infos, is_optifine)| parse_mods(name, map, infos, is_optifine))
        .try_flatten();

    let (tx, rx) = crossbeam_channel::unbounded();

    let instant = std::time::Instant::now();
    stream
        .try_for_each_concurrent(args.threads, |mod_object| {
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
    println!("Took {:?}", instant.elapsed());

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