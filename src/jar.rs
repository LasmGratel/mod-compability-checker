use std;
use std::io;
use std::io::{Read, Seek};
use std::marker::PhantomData;
use async_zip::error::ZipError;
use async_zip::read::fs::ZipFileReader;
use async_zip::read::{ZipEntry, ZipEntryReader};
use futures::stream::iter;
use futures::StreamExt;
use memmap2::Mmap;
use simd_json::Array;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek};



pub struct SeekJarFile<'a, R> where R: AsyncRead + Unpin + AsyncSeek {
    archive: async_zip::read::seek::ZipFileReader<&'a mut R>
}

impl<'a, R> SeekJarFile<'a, R> where R: AsyncRead + Unpin + AsyncSeek {
    pub async fn new(reader: &'a mut R) -> Result<SeekJarFile<'a, R>, ZipError> {
        let archive = async_zip::read::seek::ZipFileReader::new(reader).await?;
        Ok(SeekJarFile {
            archive
        })
    }

    pub async fn read_to_string(&mut self, name: &str) -> io::Result<Option<String>> {
        let (index, entry) = match self.archive.entry(name) {
            None => { return Ok(None) }
            Some(x) => { x }
        };
        let mut buf = String::with_capacity(match entry.uncompressed_size() {
            None => { return Ok(None) }
            Some(x) => { x as usize }
        });
        self.archive.entry_reader(index).await.map_err(|x| io::Error::new(io::ErrorKind::Other, x))?.read_to_string(&mut buf).await?;
        Ok(Some(buf))
    }
}

pub struct MmapJarFile<'a> {
    mmap: &'a Mmap,
    archive: async_zip::read::mem::ZipFileReader<'a>
}

impl<'a> MmapJarFile<'a> {
    pub async fn new(mmap: &'a Mmap) -> io::Result<MmapJarFile<'a>> {
        let archive = async_zip::read::mem::ZipFileReader::new(mmap).await.map_err(|x| io::Error::new(io::ErrorKind::Other, x))?;
        Ok(MmapJarFile {
            mmap,
            archive
        })
    }

    pub async fn contains(&self, name: &str) -> bool {
        futures::stream::iter(self.archive.entries().iter()).any(|x| async { x.name() == name }).await
    }

    pub async fn read_to_string(&mut self, name: &str) -> io::Result<Option<String>> {
        let (index, entry) = match self.archive.entry(name) {
            None => { return Ok(None) }
            Some(x) => { x }
        };
        let mut buf = String::with_capacity(match entry.uncompressed_size() {
            None => { return Ok(None) }
            Some(x) => { x as usize }
        });
        self.archive.entry_reader(index).await.map_err(|x| io::Error::new(io::ErrorKind::Other, x))?.read_to_string(&mut buf).await?;
        Ok(Some(buf))
    }
}
