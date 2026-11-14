//! Create an image-backed theme; DreamSkin generates its palette when applied.
use super::*;

pub(super) fn import(path: &Path, root: &Path) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "avif"
        ),
        crate::tr!("image_import.select_a_zip_or_a_png_jpg_jpeg_webp")
    );
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(crate::tr!("image_import.background"));
    let folder = theme_folder_name(name)?;
    fs::create_dir_all(root).context(crate::tr!(
        "image_import.could_not_create_the_theme_directory"
    ))?;
    // A unique folder preserves existing themes and cleans itself up on failure.
    let directory = tempfile::Builder::new()
        .prefix(&format!("{folder}-"))
        .tempdir_in(root)
        .context(crate::tr!(
            "image_import.could_not_create_the_image_theme_folder"
        ))?;
    let id = directory.path().file_name().unwrap().to_string_lossy();
    let image = format!("background.{extension}");
    fs::copy(path, directory.path().join(&image)).context(crate::tr!(
        "image_import.could_not_copy_the_background_image"
    ))?;
    let theme = json!({
        "id": id,
        "name": name,
        "appearance": "auto",
        "image": image,
    });
    // Omitting colors selects the renderer's adaptive palette for both shells.
    fs::write(
        directory.path().join("theme.json"),
        serde_json::to_vec_pretty(&theme)?,
    )
    .context(crate::tr!(
        "image_import.could_not_generate_the_image_theme_configuration"
    ))?;
    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({"themeId": id}))?,
    )
    .context(crate::tr!(
        "image_import.could_not_generate_the_image_theme_manifest"
    ))?;
    let _ = directory.keep();
    Ok(())
}
