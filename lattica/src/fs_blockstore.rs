use blockstore::{Blockstore, Error};
use cid::CidGeneric;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use std::path::PathBuf;

#[derive(Clone)]
pub struct FsBlockstore {
    base_path: PathBuf,
}

impl FsBlockstore {
    pub fn new(base_path: PathBuf) -> Result<Self, Error> {
        // Create base directory if it doesn't exist (sync, only once during init)
        std::fs::create_dir_all(&base_path)
            .map_err(|e| Error::ExecutorError(e.to_string()))?;
        
        // Pre-create common CID prefix directories to avoid runtime creation overhead
        // Most CIDs start with "ba" (bafkrei..., bafybei...)
        for prefix in ["ba", "bb", "bc", "bd", "be", "bf"] {
            let subdir = base_path.join(prefix);
            let _ = std::fs::create_dir(&subdir); // Ignore errors if already exists
        }
        
        Ok(Self { base_path })
    }
    
    fn block_path<const S: usize>(&self, cid: &CidGeneric<S>) -> PathBuf {
        let cid_str = cid.to_string();
        // Use first 2 chars as subdirectory for better filesystem performance
        let prefix = &cid_str[..2.min(cid_str.len())];
        let subdir = self.base_path.join(prefix);
        subdir.join(&cid_str)
    }
}

impl Blockstore for FsBlockstore {
    async fn get<const S: usize>(&self, cid: &CidGeneric<S>) -> Result<Option<Vec<u8>>, Error> {
        let path = self.block_path(cid);
        
        // Use async file I/O - no thread blocking, true concurrency
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::ExecutorError(e.to_string()))
        }
    }

    async fn put_keyed<const S: usize>(&self, cid: &CidGeneric<S>, data: &[u8]) -> Result<(), Error> {
        let path = self.block_path(cid);
        
        // Create parent directory if needed (async, fast no-op if exists)
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent).await; // Ignore errors if exists
        }
        
        // Use async file write - no thread blocking, maximum performance
        let mut file = File::create(&path).await
            .map_err(|e| Error::ExecutorError(e.to_string()))?;
        
        file.write_all(data).await
            .map_err(|e| Error::ExecutorError(e.to_string()))?;
        
        Ok(())
    }

    async fn remove<const S: usize>(&self, cid: &CidGeneric<S>) -> Result<(), Error> {
        let path = self.block_path(cid);
        
        // Use async remove - no thread blocking
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::ExecutorError(e.to_string()))
        }
    }

    async fn has<const S: usize>(&self, cid: &CidGeneric<S>) -> Result<bool, Error> {
        let path = self.block_path(cid);
        Ok(tokio::fs::try_exists(&path).await.unwrap_or(false))
    }
    
    async fn close(self) -> Result<(), Error> {
        // No resources to cleanup for file-based storage
        Ok(())
    }
}
