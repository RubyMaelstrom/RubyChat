//! Windows resources for rubychat.exe ("Resource File Formats"): its icon,
//! from assets/desktop/rubychat.ico, which Explorer and the taskbar show;
//! version information, whose description names RubyChat in Windows' lists;
//! and an application manifest. They're written as a .res file, which the
//! MSVC linker (and lld-link) takes as it is. Other targets build nothing.

use std::path::PathBuf;

const ICO: &str = "assets/desktop/rubychat.ico";
/// English (United States), with Unicode strings.
const LANGUAGE: u16 = 0x0409;
const CODE_PAGE: u16 = 1200;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;

/// Windows 10 and 11 (`supportedOS`), DPI per monitor, UTF-8 as the code
/// page, long paths, and version 6 of the common controls.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="RubyMaelstrom.RubyChat" version="0.0.0.0"/>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICO}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo::warning=rubychat.exe's icon and manifest need the MSVC linker");
        return;
    }
    let icon = std::fs::read(ICO).expect("assets/desktop/rubychat.ico");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("rubychat.res");
    std::fs::write(&out, resources(&icon)).expect("writing rubychat.res");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// The .res file: the empty entry that marks a 32-bit resource file, each
/// icon image, their group, the version and the manifest.
fn resources(ico: &[u8]) -> Vec<u8> {
    let mut res = Vec::new();
    resource(&mut res, 0, 0, 0, &[]);
    let word = |at: usize| u16::from_le_bytes([ico[at], ico[at + 1]]);
    let dword = |at: usize| u32::from_le_bytes([ico[at], ico[at + 1], ico[at + 2], ico[at + 3]]);
    assert_eq!(word(2), 1, "rubychat.ico is an icon file");
    let count = word(4);
    // NEWHEADER, then a RESDIR per image: an ICONDIRENTRY with the image's
    // resource ID in place of its file offset.
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&count.to_le_bytes());
    for n in 0..count {
        let entry = 6 + 16 * usize::from(n);
        let (length, offset) = (dword(entry + 8) as usize, dword(entry + 12) as usize);
        let id = n + 1;
        resource(
            &mut res,
            RT_ICON,
            id,
            LANGUAGE,
            &ico[offset..offset + length],
        );
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, LANGUAGE, &group);
    resource(&mut res, RT_VERSION, 1, LANGUAGE, &version());
    resource(&mut res, RT_MANIFEST, 1, LANGUAGE, MANIFEST.as_bytes());
    res
}

/// A RESOURCEHEADER with ordinal type and name, then the data, each
/// DWORD-aligned.
fn resource(res: &mut Vec<u8>, kind: u16, name: u16, language: u16, data: &[u8]) {
    let mut header = Vec::new();
    header.extend_from_slice(&(data.len() as u32).to_le_bytes());
    header.extend_from_slice(&32u32.to_le_bytes());
    for ordinal in [kind, name] {
        header.extend_from_slice(&0xffffu16.to_le_bytes());
        header.extend_from_slice(&ordinal.to_le_bytes());
    }
    header.extend_from_slice(&0u32.to_le_bytes());
    // Moveable, pure and discardable, as rc writes them.
    header.extend_from_slice(&0x1030u16.to_le_bytes());
    header.extend_from_slice(&language.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    debug_assert_eq!(header.len(), 32);
    res.extend_from_slice(&header);
    res.extend_from_slice(data);
    pad(res);
}

fn pad(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// VS_VERSIONINFO: the fixed file information, then the strings Windows
/// shows (StringFileInfo) and their language (VarFileInfo).
fn version() -> Vec<u8> {
    let number = |name: &str| -> u16 {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    };
    let (major, minor, patch) = (
        number("CARGO_PKG_VERSION_MAJOR"),
        number("CARGO_PKG_VERSION_MINOR"),
        number("CARGO_PKG_VERSION_PATCH"),
    );
    let text = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let most = (u32::from(major) << 16) | u32::from(minor);
    let least = u32::from(patch) << 16;
    let mut fixed = Vec::new();
    for value in [
        0xfeef_04bd, // signature
        0x0001_0000, // structure version
        most,
        least,
        most,
        least,
        0x3f,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend_from_slice(&u32::to_le_bytes(value));
    }
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "RubyMaelstrom"),
        ("FileDescription", "RubyChat"),
        ("FileVersion", text.as_str()),
        ("InternalName", "rubychat"),
        ("LegalCopyright", "RubyMaelstrom, MPL-2.0"),
        ("OriginalFilename", "rubychat.exe"),
        ("ProductName", "RubyChat"),
        ("ProductVersion", text.as_str()),
    ]
    .iter()
    .map(|(key, value)| block(key, Value::Text(value), &[]))
    .collect();
    let table = block(
        &format!("{LANGUAGE:04X}{CODE_PAGE:04X}"),
        Value::None,
        &strings,
    );
    let string_info = block("StringFileInfo", Value::None, &[table]);
    let mut translation = Vec::new();
    translation.extend_from_slice(&LANGUAGE.to_le_bytes());
    translation.extend_from_slice(&CODE_PAGE.to_le_bytes());
    let var = block("Translation", Value::Binary(&translation), &[]);
    let var_info = block("VarFileInfo", Value::None, &[var]);
    block(
        "VS_VERSION_INFO",
        Value::Binary(&fixed),
        &[string_info, var_info],
    )
}

enum Value<'a> {
    None,
    Binary(&'a [u8]),
    Text(&'a str),
}

/// One block of version information: its length, its value's length (in
/// bytes, or characters for text), whether that's text, its key, then its
/// value and children, each DWORD-aligned.
fn block(key: &str, value: Value<'_>, children: &[Vec<u8>]) -> Vec<u8> {
    let (value_bytes, value_length, text) = match value {
        Value::None => (Vec::new(), 0, 1u16),
        Value::Binary(bytes) => (bytes.to_vec(), bytes.len(), 0),
        Value::Text(text) => {
            let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
            let bytes = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
            (bytes, units.len(), 1)
        }
    };
    let mut block = vec![0, 0];
    block.extend_from_slice(&(value_length as u16).to_le_bytes());
    block.extend_from_slice(&text.to_le_bytes());
    for unit in key.encode_utf16().chain([0]) {
        block.extend_from_slice(&unit.to_le_bytes());
    }
    pad(&mut block);
    block.extend_from_slice(&value_bytes);
    for child in children {
        pad(&mut block);
        block.extend_from_slice(child);
    }
    let length = block.len() as u16;
    block[..2].copy_from_slice(&length.to_le_bytes());
    block
}
