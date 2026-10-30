//! The store thread (DESIGN.md §5): every file read and write happens here,
//! so the UI thread never waits on a disk. Writes are atomic: a temporary
//! file in the same directory, flushed to disk, then renamed over the old
//! one. Missing directories are created with mode 0700 and existing ones
//! keep their permissions (XDG Base Directory Specification, "Referencing
//! this specification"). Secrets are written readable by their owner alone,
//! and saves queued for the same file collapse into the newest.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::platform::paths;

/// A file RubyChat keeps (DESIGN.md §11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Document {
    /// `$XDG_STATE_HOME/rubychat/recent.toml`: recently used servers.
    Recent,
    /// `$XDG_CONFIG_HOME/rubychat/networks.toml`: saved networks.
    Networks,
    /// `$XDG_CONFIG_HOME/rubychat/secrets.toml`: passwords, owner-only.
    Secrets,
    /// `$XDG_CONFIG_HOME/rubychat/known_hosts`: certificate pins.
    KnownHosts,
    /// `$XDG_STATE_HOME/rubychat/session.toml`: what was open at exit.
    Session,
    /// `$XDG_CONFIG_HOME/rubychat/sts.toml`: STS policies.
    Sts,
}

impl Document {
    pub fn path(self) -> Option<PathBuf> {
        match self {
            Document::Recent => paths::state_dir().map(|dir| dir.join("recent.toml")),
            Document::Networks => paths::config_dir().map(|dir| dir.join("networks.toml")),
            Document::Secrets => paths::config_dir().map(|dir| dir.join("secrets.toml")),
            Document::KnownHosts => paths::config_dir().map(|dir| dir.join("known_hosts")),
            Document::Session => paths::state_dir().map(|dir| dir.join("session.toml")),
            Document::Sts => paths::config_dir().map(|dir| dir.join("sts.toml")),
        }
    }

    /// Readable by its owner alone.
    fn private(self) -> bool {
        self == Document::Secrets
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
                // before it, and shutdown waits for pending writes. A save
                // that a newer one of the same file follows, with no load
                // between them, is skipped.
                while let Ok(first) = receiver.recv() {
                    let mut batch = vec![first];
                    batch.extend(receiver.try_iter());
                    let skip: Vec<bool> = (0..batch.len())
                        .map(|index| superseded(&batch, index))
                        .collect();
                    for (command, skip) in batch.into_iter().zip(skip) {
                        match command {
                            Command::Load(document) => {
                                sink(StoreEvent::Loaded(document, load(document)))
                            }
                            Command::Save(..) if skip => {}
                            Command::Save(document, text) => {
                                sink(StoreEvent::Saved(document, save(document, &text)))
                            }
                            Command::Shutdown => return,
                        }
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

/// Whether the save at `index` is overtaken by a later save of the same
/// document, with no load of it between them.
fn superseded(batch: &[Command], index: usize) -> bool {
    let Command::Save(document, _) = &batch[index] else {
        return false;
    };
    batch[index + 1..]
        .iter()
        .take_while(|later| !matches!(later, Command::Load(d) if d == document))
        .any(|later| matches!(later, Command::Save(d, _) if d == document))
}

fn load(document: Document) -> Result<Option<String>, String> {
    let path = document.path().ok_or("no home directory")?;
    if document.private() {
        restrict(&path);
    }
    read(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn save(document: Document, text: &str) -> Result<(), String> {
    let path = document.path().ok_or("no home directory")?;
    write_atomic(&path, text.as_bytes(), document.private())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Makes a private file readable by its owner alone again, should someone
/// have opened it up.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path)
            && metadata.permissions().mode() & 0o077 != 0
        {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
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
/// mode 0700; a `private` file is readable by its owner alone (0600) from
/// the moment it exists.
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
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
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if private { 0o600 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = options.open(&temporary)?;
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
        write_atomic(&path, b"first", false).unwrap();
        write_atomic(&path, b"second", true).unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some("second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
            let file = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(file & 0o777, 0o600, "a private file is the owner's alone");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            restrict(&path);
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary files left");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn newer_saves_overtake_older_ones() {
        let save = |document, text: &str| Command::Save(document, text.to_string());
        let batch = [
            save(Document::Session, "a"),
            save(Document::Networks, "n"),
            save(Document::Session, "b"),
            Command::Load(Document::Session),
            save(Document::Session, "c"),
        ];
        let skipped: Vec<bool> = (0..batch.len())
            .map(|index| superseded(&batch, index))
            .collect();
        assert_eq!(
            skipped,
            [true, false, false, false, false],
            "a load in between keeps the save before it"
        );
    }
}
