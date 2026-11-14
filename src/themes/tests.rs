use super::*;
use zip::write::SimpleFileOptions;

fn archive(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, contents) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn batch_zip_import_continues_after_a_failure_and_reports_partial_success() {
    let temp = tempfile::tempdir().unwrap();
    let bad = temp.path().join("bad.zip");
    let first = temp.path().join("first.ZIP");
    let last = temp.path().join("last.zip");
    archive(&bad, &[("readme.txt", b"no manifest")]);
    archive(
        &first,
        &[
            ("manifest.json", br#"{"themeId":"first"}"#),
            ("theme.json", b"{}"),
        ],
    );
    archive(
        &last,
        &[
            ("wrapper/manifest.json", br#"{"themeId":"last"}"#),
            ("wrapper/theme.json", b"{}"),
        ],
    );
    let service = ThemeService::new(temp.path().join("state.json"));
    let root = temp.path().join("Themes");
    let error = service
        .import_files(root.to_str().unwrap(), &[first, bad, last])
        .unwrap_err()
        .to_string();
    assert!(error.contains("已导入 2 个，1 个失败"));
    assert!(error.contains("bad.zip"));
    assert!(root.join("first/theme.json").is_file());
    assert!(root.join("last/theme.json").is_file());
    assert_eq!(scan(&root).unwrap().len(), 2);
    assert!(service.inner.lock().unwrap().worker.is_none());
}

#[test]
fn image_import_builds_an_adaptive_theme_and_preserves_source_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("晚霞.PNG");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 90, 60, 255]))
        .save_with_format(&source, image::ImageFormat::Png)
        .unwrap();
    let original = fs::read(&source).unwrap();
    let root = temp.path().join("Themes");
    let service = ThemeService::new(temp.path().join("state.json"));
    service
        .import_files(root.to_str().unwrap(), std::slice::from_ref(&source))
        .unwrap();
    let snapshot = service.snapshot(root.to_str().unwrap()).unwrap();
    assert_eq!(snapshot.themes.len(), 1);
    assert_eq!(snapshot.themes[0].name, "晚霞");
    assert!(snapshot.selected.is_none());
    assert!(service.inner.lock().unwrap().worker.is_none());
    let directory = Path::new(&snapshot.themes[0].id);
    let theme: Value =
        serde_json::from_slice(&fs::read(directory.join("theme.json")).unwrap()).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["themeId"].as_str().unwrap(),
        directory.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(theme["appearance"], "auto");
    assert!(theme.get("colors").is_none());
    assert_eq!(
        fs::read(directory.join(theme["image"].as_str().unwrap())).unwrap(),
        original
    );
    let payload = payload::build(directory, Some(45)).unwrap();
    assert!(payload.contains(r#""colorMode":"auto""#));
    assert!(payload.contains(r#""userTransparency":45"#));
    assert!(payload.contains("data:image/png;base64,"));
    assert_eq!(fs::read(source).unwrap(), original);
}

#[test]
fn images_and_zips_can_be_imported_together_without_overwriting_same_name_images() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("same.jpg");
    fs::write(&source, b"original image bytes").unwrap();
    let zip = temp.path().join("theme.zip");
    archive(
        &zip,
        &[
            ("manifest.json", br#"{"themeId":"zip-theme"}"#),
            ("theme.json", b"{}"),
        ],
    );
    let root = temp.path().join("Themes");
    let service = ThemeService::new(temp.path().join("state.json"));
    let message = service
        .import_files(root.to_str().unwrap(), &[source.clone(), zip, source])
        .unwrap();
    assert!(message.contains("已导入 3 个主题"));
    let entries = scan(&root).unwrap();
    assert_eq!(entries.len(), 3);
    let images: Vec<_> = entries
        .iter()
        .filter(|entry| Path::new(&entry.id).join("background.jpg").is_file())
        .collect();
    assert_eq!(images.len(), 2);
    assert_ne!(images[0].id, images[1].id);
    for entry in images {
        assert_eq!(
            fs::read(Path::new(&entry.id).join("background.jpg")).unwrap(),
            b"original image bytes"
        );
    }
}

#[test]
fn common_image_extensions_keep_their_media_type() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Themes");
    let service = ThemeService::new(temp.path().join("state.json"));
    for (extension, mime) in [
        ("PNG", "image/png"),
        ("JPG", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
        ("bmp", "image/bmp"),
        ("avif", "image/avif"),
    ] {
        let source = temp.path().join(format!("source.{extension}"));
        fs::write(&source, b"copied without decoding at import").unwrap();
        service
            .import_files(root.to_str().unwrap(), &[source])
            .unwrap();
        let entries = scan(&root).unwrap();
        let entry = entries
            .iter()
            .find(|entry| {
                Path::new(&entry.id)
                    .join(format!("background.{}", extension.to_ascii_lowercase()))
                    .exists()
            })
            .unwrap();
        assert!(
            payload::build(Path::new(&entry.id), None)
                .unwrap()
                .contains(&format!("data:{mime};base64,"))
        );
    }
}

#[test]
fn failed_image_copy_leaves_no_incomplete_theme() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Themes");
    let service = ThemeService::new(temp.path().join("state.json"));
    let error = service
        .import_files(root.to_str().unwrap(), &[temp.path().join("missing.png")])
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing.png"));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    assert!(
        service
            .import_files(root.to_str().unwrap(), &[temp.path().join("file.txt")])
            .is_err()
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn importing_nested_theme_uses_theme_id_without_compatibility_checks() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("theme.zip");
    archive(
        &zip,
        &[
            (".metadata/manifest.json", b"ignored"),
            ("old/theme.json", b"{invalid json"),
            ("old/theme.css", b"unsupported ???"),
            (
                "old/manifest.json",
                br#"{"themeId":"imported-theme","name":"ignored-name","minClientVersion":"v999999.0","sha256":"wrong"}"#,
            ),
            ("notes/readme.txt", b"arbitrary extra files"),
        ],
    );
    let root = temp.path().join("Themes");
    let service = ThemeService::new(temp.path().join("state/theme.json"));
    service.import_zip(root.to_str().unwrap(), &zip).unwrap();
    assert_eq!(
        fs::read(root.join("imported-theme/theme.json")).unwrap(),
        b"{invalid json"
    );
    assert!(!root.join("notes").exists());
    assert!(!root.join(".metadata").exists());
    assert!(!root.join("old").exists());
    assert!(root.join("imported-theme/manifest.json").is_file());
    let snapshot = service.snapshot(root.to_str().unwrap()).unwrap();
    assert_eq!(snapshot.themes.len(), 1);
    assert_eq!(snapshot.themes[0].name, "imported-theme");
    assert!(snapshot.selected.is_none());
    assert!(service.inner.lock().unwrap().worker.is_none());
    // Invalid author metadata is reported at Apply, never during import.
    assert!(
        service
            .select(root.to_str().unwrap(), Some(snapshot.themes[0].id.clone()))
            .is_err()
    );
    assert!(service.inner.lock().unwrap().worker.is_none());
}

#[test]
fn root_manifest_takes_priority_and_extracts_all_contents_into_theme_id() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("theme.zip");
    archive(
        &zip,
        &[
            ("nested/manifest.json", br#"{"themeId":"nested"}"#),
            ("manifest.json", br#"{"themeId":"root-theme"}"#),
            ("theme.json", b"{}"),
            ("assets/background.png", b"image"),
        ],
    );
    let root = temp.path().join("Themes");
    extract_zip(&zip, &root).unwrap();
    assert!(root.join("root-theme/manifest.json").is_file());
    assert!(root.join("root-theme/theme.json").is_file());
    assert!(root.join("root-theme/nested/manifest.json").is_file());
    assert_eq!(
        fs::read(root.join("root-theme/assets/background.png")).unwrap(),
        b"image"
    );
    assert!(!root.join("nested").exists());
}

#[test]
fn importing_requires_manifest_in_root_or_first_visible_directory() {
    let temp = tempfile::tempdir().unwrap();
    for entries in [
        vec![("theme.json", b"{}".as_slice())],
        vec![
            ("first/readme.txt", b"first".as_slice()),
            (
                "second/manifest.json",
                br#"{"themeId":"second"}"#.as_slice(),
            ),
        ],
        vec![(
            ".hidden/manifest.json",
            br#"{"themeId":"hidden"}"#.as_slice(),
        )],
        vec![(
            "first/deeper/manifest.json",
            br#"{"themeId":"deep"}"#.as_slice(),
        )],
    ] {
        let zip = temp.path().join("theme.zip");
        archive(&zip, &entries);
        let root = temp.path().join("Themes");
        assert!(extract_zip(&zip, &root).is_err());
        assert!(!root.exists());
    }
}

#[test]
fn importing_requires_parseable_manifest_with_theme_id() {
    let temp = tempfile::tempdir().unwrap();
    for manifest in [
        "invalid JSON",
        r#"{"name":"not-the-id"}"#,
        r#"{"themeId":"  "}"#,
        r#"{"themeId":123}"#,
    ] {
        let zip = temp.path().join("theme.zip");
        archive(&zip, &[("manifest.json", manifest.as_bytes())]);
        let root = temp.path().join("Themes");
        assert!(extract_zip(&zip, &root).is_err());
        assert!(!root.exists());
    }
}

#[test]
fn manual_nested_themes_are_discovered_without_css_or_manifest() {
    let temp = tempfile::tempdir().unwrap();
    for dir in ["outer/one", "outer/two"] {
        fs::create_dir_all(temp.path().join(dir)).unwrap();
        fs::write(
            temp.path().join(dir).join("theme.json"),
            br#"{"displayName":"Same","version":"old"}"#,
        )
        .unwrap();
    }
    let entries = scan(temp.path()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0].name, entries[1].name);
    assert!(
        entries
            .iter()
            .all(|entry| Path::new(&entry.id).is_absolute())
    );
}

#[test]
fn extraction_does_not_write_outside_the_chosen_directory() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("escape.zip");
    archive(
        &zip,
        &[
            ("manifest.json", br#"{"themeId":"safe"}"#),
            ("../outside.txt", b"no"),
        ],
    );
    assert!(extract_zip(&zip, &temp.path().join("Themes")).is_err());
    assert!(!temp.path().join("outside.txt").exists());
    assert!(!temp.path().join("Themes/outside.txt").exists());
}

#[test]
fn nested_extraction_cannot_escape_its_theme_folder() {
    let temp = tempfile::tempdir().unwrap();
    let zip = temp.path().join("escape.zip");
    archive(
        &zip,
        &[
            ("wrapper/manifest.json", br#"{"themeId":"safe"}"#),
            ("wrapper/../outside.txt", b"no"),
        ],
    );
    let root = temp.path().join("Themes");
    assert!(extract_zip(&zip, &root).is_err());
    assert!(!root.join("outside.txt").exists());
}

#[test]
fn theme_id_is_made_into_a_single_filesystem_folder() {
    assert_eq!(theme_folder_name("星空主题").unwrap(), "星空主题");
    assert_eq!(theme_folder_name("../outside").unwrap(), ".._outside");
    assert_eq!(theme_folder_name("CON").unwrap(), "_CON");
    assert_eq!(theme_folder_name("theme: night. ").unwrap(), "theme_ night");
    assert!(theme_folder_name("..").is_err());
}

#[test]
fn off_is_persisted_without_starting_a_worker() {
    let temp = tempfile::tempdir().unwrap();
    let service = ThemeService::new(temp.path().join("theme-state.json"));
    service.select(temp.path().to_str().unwrap(), None).unwrap();
    assert!(service.inner.lock().unwrap().worker.is_none());
    let saved: Value =
        serde_json::from_slice(&fs::read(temp.path().join("theme-state.json")).unwrap()).unwrap();
    assert!(saved["selected"].is_null());
    assert!(service.set_transparency(30).is_err());
    let arguments = debug_arguments(&temp.path().join("browser-profile"));
    assert!(
        arguments
            .iter()
            .any(|arg| arg == "--remote-debugging-address=127.0.0.1")
    );
    assert!(
        arguments
            .iter()
            .any(|arg| arg == "--remote-debugging-port=9347")
    );
    assert!(
        arguments
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--user-data-dir="))
    );
}

#[test]
fn default_directory_is_relative_to_executable_instead_of_working_directory() {
    assert_eq!(
        theme_directory("").unwrap(),
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("Themes")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn bundled_application_keeps_imported_themes_outside_the_signed_bundle() {
    let executable = Path::new("/Applications/Codex Switch.app/Contents/MacOS/codex-switch");
    assert_eq!(
        default_theme_directory(executable).unwrap(),
        PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("Library/Application Support/Codex Switch/Themes")
    );
    assert_eq!(
        default_theme_directory(Path::new("/tmp/Contents/MacOS/codex-switch")).unwrap(),
        Path::new("/tmp/Contents/MacOS/Themes")
    );
}

#[test]
fn transparency_dialog_starts_at_author_alpha_until_user_overrides_it() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("theme.json"),
        br#"{"colors":{"panel":"rgba(1,2,3,0.25)"}}"#,
    )
    .unwrap();
    assert_eq!(payload::default_transparency(temp.path()), 75);
    fs::write(
        temp.path().join("theme.json"),
        r##"{"colors":{"panel":"#1238"}}"##,
    )
    .unwrap();
    assert_eq!(payload::default_transparency(temp.path()), 47);
    fs::write(
        temp.path().join("theme.json"),
        r##"{"colors":{"panel":"#😀"}}"##,
    )
    .unwrap();
    assert_eq!(payload::default_transparency(temp.path()), 30);
}

#[test]
fn unreadable_library_disables_previous_selection_and_records_off() {
    let temp = tempfile::tempdir().unwrap();
    let service = ThemeService::new(temp.path().join("theme-state.json"));
    {
        let mut library = service.inner.lock().unwrap();
        library.restored = true;
        library.selected = Some("previous-theme".into());
    }
    let path = temp.path().join("not-a-directory");
    fs::write(&path, "file").unwrap();
    assert!(service.snapshot(path.to_str().unwrap()).is_err());
    assert!(service.inner.lock().unwrap().selected.is_none());
    let saved: Value =
        serde_json::from_slice(&fs::read(temp.path().join("theme-state.json")).unwrap()).unwrap();
    assert!(saved["selected"].is_null());
}

#[test]
fn browser_storage_cannot_appear_as_a_configuration_backup() {
    let temp = tempfile::tempdir().unwrap();
    let browser = browser_directory(&temp.path().join("backups/theme-state.json"));
    fs::create_dir_all(browser.join("123")).unwrap();
    fs::write(browser.join("Local State"), "browser storage").unwrap();
    let store = crate::store::Store::new(temp.path().to_path_buf());
    assert!(store.load().unwrap().profiles.is_empty());
}
