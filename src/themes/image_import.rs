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
        "请选择 ZIP 或 PNG、JPG、JPEG、WebP、GIF、BMP、AVIF 图片"
    );
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("背景图");
    let folder = theme_folder_name(name)?;
    fs::create_dir_all(root).context("无法创建主题目录")?;
    // A unique folder preserves existing themes and cleans itself up on failure.
    let directory = tempfile::Builder::new()
        .prefix(&format!("{folder}-"))
        .tempdir_in(root)
        .context("无法创建图片主题文件夹")?;
    let id = directory.path().file_name().unwrap().to_string_lossy();
    let image = format!("background.{extension}");
    fs::copy(path, directory.path().join(&image)).context("无法复制背景图片")?;
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
    .context("无法生成图片主题配置")?;
    fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&json!({"themeId": id}))?,
    )
    .context("无法生成图片主题清单")?;
    let _ = directory.keep();
    Ok(())
}
