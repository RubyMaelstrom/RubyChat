//! The store thread (DESIGN.md §5): every file read and write happens here,
//! so the UI thread never waits on a disk. Writes are atomic: a temporary
//! file in the same directory, flushed to disk, then renamed over the old
//! one. Missing directories are created with mode 0700 and existing ones
//! keep their permissions (XDG Base Directory Specification, "Referencing
//! this specification").

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::platform::paths;

/// A file RubyChat keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Document {
    /// `$XDG_STATE_HOME/rubychat/recent.toml`: recently used servers.
    Recent,
}

impl Document {
    pub fn path(self) -> Option<PathBuf> {
        match self {
            Document::Recent => paths::state_dir().map(|dir| dir.join("recent.toml")),
        }
    }
}

enum Command {
    Load(Document),
    Save(Document, String),
    Shutdown,
}

#[derive(Debug)]
pub enum StoreEvent {
    /// The document's text; `None` if it doesn't exist yet.
    Loaded(Document, Result<Option<String>, String>),
    Saved(Document, Result<(), String>),
}

pub struct Store {
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

/// Documents larger than this are refused as damaged.
const MAX_BYTES: u64 = 4 << 20;

impl Store {
    /// Starts the store thread; `sink` receives its events on that thread.
    pub fn spawn(mut sink: impl FnMut(StoreEvent) + Send + 'static) -> io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(String::from("rubychat-store"))
            .spawn(move || {
                // Commands run in order, so a save always follows the load
                // before it, and shutdown waits for pending writes.
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Load(document) => {
                            sink(StoreEvent::Loaded(document, load(document)))
                        }
                        Command::Save(document, text) => {
                            sink(StoreEvent::Saved(document, save(document, &text)))
                        }
                        Command::Shutdown => break,
                    }
                }
            })?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    pub fn load(&self, document: Document) {
        let _ = self.commands.send(Command::Load(document));
    }

    pub fn save(&self, document: Document, text: String) {
        let _ = self.commands.send(Command::Save(document, text));
    }
}

impl Drop for Store {
    /// Finishes pending writes before the app exits.
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn load(document: Document) -> Result<Option<String>, String> {
    let path = document.path().ok_or("no home directory")?;
    read(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn save(document: Document, text: &str) -> Result<(), String> {
    let path = document.path().ok_or("no home directory")?;
    write_atomic(&path, text.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if metadata.len() > MAX_BYTES => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
        }
        Ok(_) => {}
    }
    let bytes = fs::read(path)?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// Writes `bytes` to `path` atomically, creating missing directories with
/// mode 0700.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no directory"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    // Make the rename itself durable.
    if let Ok(directory) = fs::File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_atomically_into_new_private_directories() {
        let root = std::env::temp_dir().join(format!("rubychat-store-{}", std::process::id()));
        let path = root.join("state/rubychat/recent.toml");
        assert_eq!(read(&path).unwrap(), None);
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary files left");
        fs::remove_dir_all(&root).unwrap();
    }
}
