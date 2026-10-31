//! The font worker (DESIGN.md §9.5): indexes the installed fonts and loads
//! fallback faces, off the UI thread.
//!
//! The index records each face's family, weight, style and the characters
//! it maps, and is cached in `$XDG_CACHE_HOME/rubychat/fonts.index`, a line
//! per coverage and per file, keyed by each file's size and modification
//! time: only new or changed files are read again. Font files are mapped,
//! not read, so a large collection costs only the pages in use.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::UNIX_EPOCH;

use skrifa::raw::FileRef;
use skrifa::string::StringId;
use skrifa::{GlyphId, MetadataProvider, Tag};

use crate::render::emoji::{ColorGlyph, color_glyph};
use crate::render::fallback::{Coverage, FaceInfo, FaceKey, FontIndex, preferred_families};
use crate::render::font::Face;
use crate::render::text::ColorRequest;

use super::mmap::Mapped;

const CACHE_FILE: &str = "fonts.index";
const CACHE_HEADER: &str = "rubychat fonts 1";
/// Font files indexed, at most, and how deep directories are searched.
const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 8;

pub enum FontCommand {
    Load(FaceKey),
    /// Decode a color glyph of a face loaded before.
    Color(ColorRequest),
}

pub enum FontEvent {
    /// The installed faces, once.
    Index(Arc<FontIndex>),
    /// A face asked for, or why it couldn't load.
    Loaded(FaceKey, Result<Box<Face>, String>),
    /// A color glyph asked for, or `None` when it has no bitmap.
    Color(ColorRequest, Option<ColorGlyph>),
}

/// The worker's handle; dropping it lets the thread finish.
pub struct Fonts {
    commands: mpsc::Sender<FontCommand>,
}

/// The user's locale where the environment doesn't say, as `ll_CC`:
/// Windows names it `ll-CC` (GetUserDefaultLocaleName).
#[cfg(windows)]
fn system_language() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH.
    let mut name = [0u16; 85];
    // SAFETY: the buffer and its length agree, and it outlives the call.
    let length = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), name.len() as i32) };
    if length <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&name[..length as usize - 1]).replace('-', "_")
}

#[cfg(not(windows))]
fn system_language() -> String {
    String::new()
}

impl Fonts {
    /// Starts the worker: it indexes the fonts, sends the index, then
    /// loads faces as asked.
    pub fn start(mut sink: impl FnMut(FontEvent) + Send + 'static) -> io::Result<Self> {
        let (commands, inbox) = mpsc::channel();
        thread::Builder::new()
            .name(String::from("rubychat-fonts"))
            .spawn(move || {
                let language = std::env::var("LC_ALL")
                    .or_else(|_| std::env::var("LC_CTYPE"))
                    .or_else(|_| std::env::var("LANG"))
                    .unwrap_or_else(|_| system_language());
                let cache = super::paths::cache_dir().map(|dir| dir.join(CACHE_FILE));
                let mut index = build_index(&super::paths::font_dirs(), cache.as_deref());
                index.preferred = preferred_families(&language);
                sink(FontEvent::Index(Arc::new(index)));
                // The data of faces loaded, for decoding their color glyphs.
                let mut loaded: HashMap<FaceKey, &'static [u8]> = HashMap::new();
                while let Ok(command) = inbox.recv() {
                    match command {
                        FontCommand::Load(key) => {
                            let face = load(&key).map(|(data, face)| {
                                loaded.insert(key.clone(), data);
                                face
                            });
                            sink(FontEvent::Loaded(key, face));
                        }
                        FontCommand::Color(request) => {
                            let glyph = loaded.get(&request.face).and_then(|data| {
                                let font =
                                    skrifa::FontRef::from_index(data, request.face.index).ok()?;
                                color_glyph(&font, request.glyph, request.ppem as f32 / 64.0)
                            });
                            sink(FontEvent::Color(request, glyph));
                        }
                    }
                }
            })?;
        Ok(Self { commands })
    }

    pub fn load(&self, key: FaceKey) {
        let _ = self.commands.send(FontCommand::Load(key));
    }

    pub fn decode(&self, request: ColorRequest) {
        let _ = self.commands.send(FontCommand::Color(request));
    }
}

/// Maps face `key` for the life of the process and reads it.
fn load(key: &FaceKey) -> Result<(&'static [u8], Box<Face>), String> {
    let mapped =
        Mapped::open(&key.path).map_err(|error| format!("{}: {error}", key.path.display()))?;
    let data = mapped.leak();
    Face::from_index(data, key.index)
        .map(|face| (data, Box::new(face)))
        .ok_or_else(|| format!("{}: not a font RubyChat can read", key.path.display()))
}

/// A file's identity for the cache: its size and modification time.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((metadata.len(), modified))
}

/// Font files under `dirs`, sorted, following links but never a directory
/// twice.
fn font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut seen = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = dirs.iter().map(|dir| (dir.clone(), 0)).collect();
    while let Some((dir, depth)) = pending.pop() {
        let Ok(canonical) = fs::canonicalize(&dir) else {
            continue;
        };
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if is_font(&path) && files.len() < MAX_FILES {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

fn is_font(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["ttf", "otf", "ttc", "otc"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Coverages shared between faces that map the same characters, as a
/// collection's weights do.
#[derive(Default)]
struct Coverages {
    known: HashMap<Arc<Coverage>, ()>,
}

impl Coverages {
    fn intern(&mut self, coverage: Coverage) -> Arc<Coverage> {
        let coverage = Arc::new(coverage);
        match self.known.get_key_value(&coverage) {
            Some((known, ())) => known.clone(),
            None => {
                self.known.insert(coverage.clone(), ());
                coverage
            }
        }
    }
}

/// Every face of the font file at `path`.
fn read_file(path: &Path, coverages: &mut Coverages) -> Vec<FaceInfo> {
    let Ok(mapped) = Mapped::open(path) else {
        return Vec::new();
    };
    let Ok(file) = FileRef::new(mapped.data()) else {
        return Vec::new();
    };
    let mut faces = Vec::new();
    for (index, font) in file.fonts().enumerate() {
        let Ok(font) = font else {
            continue;
        };
        let name = |id| {
            font.localized_strings(id)
                .english_or_first()
                .map(|name| name.chars().collect::<String>())
                .filter(|name| !name.trim().is_empty())
        };
        let Some(family) =
            name(StringId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| name(StringId::FAMILY_NAME))
        else {
            continue;
        };
        let attributes = font.attributes();
        let coverage = Coverage::from_codepoints(
            font.charmap()
                .mappings()
                .filter(|(_, glyph)| *glyph != GlyphId::NOTDEF)
                .map(|(codepoint, _)| codepoint),
        );
        let metrics = font.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        faces.push(FaceInfo {
            key: FaceKey {
                path: path.to_path_buf(),
                index: index as u32,
            },
            family: family.replace(['\t', '\n', '\r'], " "),
            weight: attributes.weight.value().clamp(1.0, 1000.0) as u16,
            italic: attributes.style != skrifa::attribute::Style::Normal,
            monospace: metrics.is_monospace,
            color: font.table_data(Tag::new(b"CBDT")).is_some(),
            coverage: coverages.intern(coverage),
        });
    }
    faces
}

/// The index of the fonts under `dirs`, reusing what `cache` says of files
/// that haven't changed, and writing it back when anything did.
fn build_index(dirs: &[PathBuf], cache: Option<&Path>) -> FontIndex {
    let mut coverages = Coverages::default();
    let mut cached = cache
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| parse_cache(&text, &mut coverages))
        .unwrap_or_default();
    let mut changed = false;
    let mut files: Vec<(PathBuf, (u64, u64), Vec<FaceInfo>)> = Vec::new();
    let found = font_files(dirs);
    for path in &found {
        let Some(stamp) = stamp(path) else {
            continue;
        };
        let faces = match cached.remove(path) {
            Some((was, faces)) if was == stamp => faces,
            _ => {
                changed = true;
                read_file(path, &mut coverages)
            }
        };
        files.push((path.clone(), stamp, faces));
    }
    // Files gone since count as a change too.
    changed |= !cached.is_empty();
    if changed && let Some(cache) = cache {
        let _ = write_cache(cache, &files);
    }
    FontIndex {
        faces: files.into_iter().flat_map(|(_, _, faces)| faces).collect(),
        preferred: Vec::new(),
    }
}

type Cached = HashMap<PathBuf, ((u64, u64), Vec<FaceInfo>)>;

/// The cache's files and their faces; `None` if it's of another version or
/// malformed, so everything is read again.
fn parse_cache(text: &str, coverages: &mut Coverages) -> Option<Cached> {
    let mut lines = text.lines();
    if lines.next()? != CACHE_HEADER {
        return None;
    }
    let mut pool: Vec<Arc<Coverage>> = Vec::new();
    let mut files: Cached = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["coverage", ranges] => {
                let ranges = ranges
                    .split(' ')
                    .filter(|range| !range.is_empty())
                    .map(|range| {
                        let (start, end) = range.split_once('-').unwrap_or((range, range));
                        Some((
                            u32::from_str_radix(start, 16).ok()?,
                            u32::from_str_radix(end, 16).ok()?,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?;
                pool.push(coverages.intern(Coverage::from_ranges(ranges)?));
            }
            ["file", size, modified, path] => {
                let path = PathBuf::from(path);
                files.insert(
                    path.clone(),
                    ((size.parse().ok()?, modified.parse().ok()?), Vec::new()),
                );
                current = Some(path);
            }
            [
                "face",
                index,
                weight,
                italic,
                monospace,
                color,
                coverage,
                family,
            ] => {
                let path = current.clone()?;
                let face = FaceInfo {
                    key: FaceKey {
                        path: path.clone(),
                        index: index.parse().ok()?,
                    },
                    family: family.to_string(),
                    weight: weight.parse().ok()?,
                    italic: *italic == "1",
                    monospace: *monospace == "1",
                    color: *color == "1",
                    coverage: pool.get(coverage.parse::<usize>().ok()?)?.clone(),
                };
                files.get_mut(&path)?.1.push(face);
            }
            _ => return None,
        }
    }
    Some(files)
}

fn write_cache(path: &Path, files: &[(PathBuf, (u64, u64), Vec<FaceInfo>)]) -> io::Result<()> {
    let mut out = String::from(CACHE_HEADER);
    out.push('\n');
    let mut numbers: HashMap<*const Coverage, usize> = HashMap::new();
    for (_, _, faces) in files {
        for face in faces {
            let key = Arc::as_ptr(&face.coverage);
            if numbers.contains_key(&key) {
                continue;
            }
            numbers.insert(key, numbers.len());
            out.push_str("coverage\t");
            for (n, (start, end)) in face.coverage.ranges().iter().enumerate() {
                if n > 0 {
                    out.push(' ');
                }
                if start == end {
                    out.push_str(&format!("{start:x}"));
                } else {
                    out.push_str(&format!("{start:x}-{end:x}"));
                }
            }
            out.push('\n');
        }
    }
    for (file, (size, modified), faces) in files {
        // A path that can't be written on one line is read again next time.
        let Some(name) = file
            .to_str()
            .filter(|name| !name.contains(['\t', '\n', '\r']))
        else {
            continue;
        };
        out.push_str(&format!("file\t{size}\t{modified}\t{name}\n"));
        for face in faces {
            out.push_str(&format!(
                "face\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                face.key.index,
                face.weight,
                u8::from(face.italic),
                u8::from(face.monospace),
                u8::from(face.color),
                numbers[&Arc::as_ptr(&face.coverage)],
                face.family
            ));
        }
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("index.new");
    fs::write(&temporary, out)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rubychat-fonts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn indexes_fonts_and_caches_what_it_read() {
        let dir = scratch("index");
        let fonts = dir.join("fonts/nested");
        fs::create_dir_all(&fonts).unwrap();
        fs::write(
            fonts.join("Mono.ttf"),
            crate::render::font::JETBRAINS_MONO_REGULAR,
        )
        .unwrap();
        fs::write(
            fonts.join("Bold.TTF"),
            crate::render::font::JETBRAINS_MONO_BOLD,
        )
        .unwrap();
        fs::write(fonts.join("notes.txt"), b"not a font").unwrap();
        fs::write(fonts.join("broken.otf"), b"not a font either").unwrap();
        let cache = dir.join("cache/fonts.index");
        let index = build_index(&[dir.join("fonts")], Some(&cache));
        assert_eq!(index.faces.len(), 2, "{:?}", index.faces);
        let regular = index.faces.iter().find(|face| face.weight == 400).unwrap();
        assert_eq!(regular.family, "JetBrains Mono");
        assert!(regular.monospace && !regular.italic && !regular.color);
        assert!(regular.coverage.contains('\u{e9}'));
        assert!(index.faces.iter().any(|face| face.weight == 700));
        // The cache holds the same, and is used: a changed file is read
        // again, the rest comes from the cache.
        let text = fs::read_to_string(&cache).unwrap();
        assert!(text.starts_with(CACHE_HEADER), "{text}");
        let again = build_index(&[dir.join("fonts")], Some(&cache));
        assert_eq!(again.faces, index.faces);
        let mut coverages = Coverages::default();
        let parsed = parse_cache(&text, &mut coverages).unwrap();
        assert_eq!(parsed.len(), 3, "every font-like file, faces or not");
        assert!(parse_cache("rubychat fonts 0\n", &mut coverages).is_none());
        assert!(parse_cache(&format!("{CACHE_HEADER}\nnonsense\n"), &mut coverages).is_none());
        // Loading maps the file and reads the face.
        let (_, face) = load(&regular.key).unwrap();
        assert!(face.coverage.contains('M'));
        assert!(
            load(&FaceKey {
                path: fonts.join("broken.otf"),
                index: 0
            })
            .is_err()
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}

/// `cargo test --release --lib installed_fonts -- --ignored --nocapture`
/// indexes this machine's fonts into a scratch cache, then again from it.
#[cfg(test)]
#[test]
#[ignore = "reads the installed fonts; run with --nocapture"]
fn installed_fonts() {
    let cache =
        std::env::temp_dir().join(format!("rubychat-fonts-installed-{}", std::process::id()));
    let _ = fs::remove_file(&cache);
    for pass in ["read", "cached"] {
        let started = std::time::Instant::now();
        let index = build_index(&super::paths::font_dirs(), Some(&cache));
        let coverages: std::collections::HashSet<*const Coverage> = index
            .faces
            .iter()
            .map(|face| Arc::as_ptr(&face.coverage))
            .collect();
        eprintln!(
            "{pass}: {} faces, {} distinct coverages in {:.1} ms",
            index.faces.len(),
            coverages.len(),
            started.elapsed().as_secs_f64() * 1e3
        );
    }
    eprintln!(
        "cache: {} bytes",
        fs::metadata(&cache).map_or(0, |metadata| metadata.len())
    );
    let _ = fs::remove_file(&cache);
}
