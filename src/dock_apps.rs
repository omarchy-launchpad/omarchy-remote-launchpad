//! Persistent macOS app bundles for remote desktop entries.
use crate::{config, config::Host, remote::App};
use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::{NSBitmapImageRep, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage};
use objc2_foundation::{NSData, NSPoint, NSRect, NSSize};
use serde_json::json;
use std::{
    ffi::CString,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::Command,
};

const EXECUTABLE: &str = "remote-viewer";
const METADATA: &str = "Contents/Resources/desktop-entry.json";
const APP_ICON: &str = "Contents/Resources/App.png";
const DOCK_ICON: &str = "Contents/Resources/App.icns";
const LOCALIZED_INFO: &str = "Contents/Resources/en.lproj/InfoPlist.strings";
pub(crate) const FALLBACK_ICON: &[u8] = include_bytes!("../packaging/omarchy-icon.png");

pub(crate) fn icon_image(bytes: &[u8]) -> Option<Retained<NSImage>> {
    // NSData copies the complete image before AppKit decodes it.
    let data = unsafe { NSData::dataWithBytes_length(bytes.as_ptr().cast(), bytes.len()) };
    NSImage::initWithData(NSImage::alloc(), &data)
}

fn badged_icon(icon: &NSImage) -> io::Result<Retained<NSData>> {
    const SIDE: isize = 256;
    const BADGE: f64 = 88.;
    const INSET: f64 = 8.;
    let logo = icon_image(FALLBACK_ICON).expect("bundled Omarchy icon is valid PNG");
    // Null planes request AppKit-owned storage for the RGBA bitmap.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), SIDE, SIDE, 8, 4, true, false,
            NSDeviceRGBColorSpace, 0, 0,
        )
    }
    .ok_or_else(|| io::Error::other("Could not allocate Dock icon bitmap"))?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
        .ok_or_else(|| io::Error::other("Could not create Dock icon drawing context"))?;
    let side = SIDE as f64;
    let size = icon.size();
    let scale = side / size.width.max(size.height);
    let width = size.width * scale;
    let height = size.height * scale;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    icon.drawInRect(NSRect::new(
        NSPoint::new((side - width) / 2., (side - height) / 2.),
        NSSize::new(width, height),
    ));
    logo.drawInRect(NSRect::new(
        NSPoint::new(side - BADGE - INSET, INSET),
        NSSize::new(BADGE, BADGE),
    ));
    NSGraphicsContext::restoreGraphicsState_class();
    bitmap
        .TIFFRepresentation()
        .ok_or_else(|| io::Error::other("Could not encode Dock icon bitmap"))
}

pub fn open(host: &Host, app: &App) -> Result<(), String> {
    let root = config::directory().join("Remote Apps");
    let bundle = prepare(
        &root,
        &std::env::current_exe().map_err(|e| e.to_string())?,
        host,
        app,
    )
    .map_err(|e| format!("Could not prepare {}: {e}", app.name))?;
    run_tool(Command::new("/usr/bin/open").arg(bundle))
        .map_err(|e| format!("Could not open {}: {e}", app.name))
}

pub fn entry() -> Option<Result<(Host, App), String>> {
    let exe = std::env::current_exe().ok()?;
    if exe.file_name()? != EXECUTABLE {
        return None;
    }
    Some((|| {
        let bundle = exe.ancestors().nth(3).ok_or("Missing viewer bundle")?;
        let (host, id, name, _) = read_metadata(bundle).map_err(|e| e.to_string())?;
        Ok((
            Host::parse(&host)?,
            App {
                id,
                name,
                icon: fs::read(bundle.join(DOCK_ICON))
                    .or_else(|_| fs::read(bundle.join(APP_ICON)))
                    .ok()
                    .map(Into::into),
            },
        ))
    })())
}

pub(crate) fn bundle_id(host: &Host, app: &App) -> String {
    // Stable FNV-1a over the host and desktop ID. Check the stored pair before
    // reusing the result so a collision can never launch a different app.
    let key = host
        .name()
        .bytes()
        .chain([0])
        .chain(app.id.bytes())
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("dev.app-omarchy-launchpad.viewer.{key:016x}")
}

fn read_metadata(bundle: &Path) -> io::Result<(String, String, String, String)> {
    Ok(serde_json::from_slice(&fs::read(bundle.join(METADATA))?)?)
}

pub(crate) fn prepare(root: &Path, source: &Path, host: &Host, app: &App) -> io::Result<PathBuf> {
    let id = bundle_id(host, app);
    let bundle = root.join(format!("{id}.app"));
    let saved_icon = fs::read(bundle.join(APP_ICON)).ok();
    let source_meta = fs::metadata(source)?;
    let revision = format!(
        "3:{}:{}:{}:{}:{}",
        source_meta.dev(),
        source_meta.ino(),
        source_meta.len(),
        source_meta.mtime(),
        source_meta.mtime_nsec()
    );
    match read_metadata(&bundle) {
        Ok((saved_host, saved_id, name, saved_revision)) => {
            if saved_host != host.name() || saved_id != app.id {
                return Err(io::Error::other("Remote app bundle ID collision"));
            }
            if saved_revision == revision
                && name == app.name
                && app
                    .icon
                    .as_deref()
                    .is_none_or(|icon| saved_icon.as_deref() == Some(icon))
                && bundle.join("Contents/MacOS").join(EXECUTABLE).is_file()
                && bundle.join(DOCK_ICON).is_file()
                && bundle.join(LOCALIZED_INFO).is_file()
            {
                return Ok(bundle);
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    fs::create_dir_all(root)?;
    let stage = root.join(format!(".{}", crate::remote::token()?));
    fs::create_dir(&stage)?;
    let staged_bundle = stage.join(bundle.file_name().unwrap());
    let result = (|| {
        let macos = staged_bundle.join("Contents/MacOS");
        let resources = staged_bundle.join("Contents/Resources");
        fs::create_dir_all(&macos)?;
        fs::create_dir_all(&resources)?;
        fs::copy(source, macos.join(EXECUTABLE))?;
        fs::write(
            staged_bundle.join(METADATA),
            serde_json::to_vec(&(host.name(), &app.id, &app.name, revision))?,
        )?;
        // macOS uses the localized display name when the base name matches the
        // bundle filename. The filename stays stable so existing Dock pins work.
        write_plist(
            &staged_bundle.join("Contents/Info.plist"),
            json!({
                "CFBundleExecutable": EXECUTABLE,
                "CFBundleIdentifier": id,
                "CFBundleName": app.name,
                "CFBundleDisplayName": id,
                "CFBundleDevelopmentRegion": "en",
                "CFBundlePackageType": "APPL",
                "CFBundleIconFile": "App.icns",
                "CFBundleVersion": "1",
                "CFBundleShortVersionString": env!("CARGO_PKG_VERSION"),
                "LSMinimumSystemVersion": "13.0",
                "NSHighResolutionCapable": true,
                "NSPrincipalClass": "NSApplication",
            }),
        )?;
        let localized = staged_bundle.join(LOCALIZED_INFO);
        fs::create_dir_all(localized.parent().unwrap())?;
        write_plist(
            &localized,
            json!({
                "CFBundleName": app.name,
                "CFBundleDisplayName": app.name,
            }),
        )?;

        let png = staged_bundle.join(APP_ICON);
        let original = app
            .icon
            .as_deref()
            .or(saved_icon.as_deref())
            .unwrap_or(FALLBACK_ICON);
        let dock_input = if let Some(icon) = (original != FALLBACK_ICON)
            .then(|| icon_image(original))
            .flatten()
            .filter(|icon| icon.isValid())
        {
            fs::write(&png, original)?;
            let tiff = stage.join("Dock.tiff");
            fs::write(&tiff, badged_icon(&icon)?.to_vec())?;
            tiff
        } else {
            fs::write(&png, FALLBACK_ICON)?;
            png
        };
        run_tool(
            Command::new("/usr/bin/sips")
                .args(["-z", "256", "256", "-s", "format", "icns"])
                .arg(dock_input)
                .arg("--out")
                .arg(staged_bundle.join(DOCK_ICON)),
        )?;
        run_tool(
            Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&staged_bundle),
        )?;

        if bundle.exists() {
            swap_bundles(&staged_bundle, &bundle)
        } else {
            fs::rename(&staged_bundle, &bundle)
        }
    })();
    let _ = fs::remove_dir_all(stage);
    result?;
    Ok(bundle)
}

fn write_plist(path: &Path, values: serde_json::Value) -> io::Result<()> {
    fs::write(path, serde_json::to_vec(&values)?)?;
    run_tool(
        Command::new("/usr/bin/plutil")
            .args(["-convert", "xml1"])
            .arg(path),
    )
}

fn run_tool(command: &mut Command) -> io::Result<()> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} ({}): {}",
        command.get_program().to_string_lossy(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn swap_bundles(staged: &Path, installed: &Path) -> io::Result<()> {
    unsafe extern "C" {
        fn renamex_np(
            from: *const std::ffi::c_char,
            to: *const std::ffi::c_char,
            flags: u32,
        ) -> i32;
    }
    const RENAME_SWAP: u32 = 0x00000002;
    let staged = CString::new(staged.as_os_str().as_bytes())?;
    let installed = CString::new(installed.as_os_str().as_bytes())?;
    // Both paths are on the same volume. A swap keeps the pinned path valid
    // throughout an update and leaves the previous copy in the staging directory.
    if unsafe { renamex_np(staged.as_ptr(), installed.as_ptr(), RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
