//! Regenerate the saved icon assets with `cargo run --example generate-icon`.

use anyhow::{Context, Result};
use resvg::{
    tiny_skia::{Pixmap, PixmapPaint, Transform},
    usvg,
};
use std::{fs, io::Write, path::Path};

const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const PREVIEW: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="420">
  <rect width="960" height="420" fill="#F2F4F6"/>
  <rect x="480" width="480" height="420" fill="#141A21"/>
  <g font-family="Segoe UI, sans-serif" font-size="16">
    <text x="44" y="48" fill="#202A35">Codex Switch</text>
    <text x="524" y="48" fill="#F4F7FA">Actual sizes</text>
    <g fill="#8995A2" font-size="13">
      <text x="525" y="180">48 px</text><text x="638" y="180">32 px</text>
      <text x="738" y="180">24 px</text><text x="832" y="180">16 px</text>
    </g>
  </g>
  <rect x="512" y="224" width="416" height="136" rx="12" fill="#F2F4F6"/>
</svg>"##;

fn main() -> Result<()> {
    // Anchor paths to the project rather than the caller's working directory.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut options = usvg::Options {
        resources_dir: Some(root.join("assets")),
        ..Default::default()
    };
    options.fontdb_mut().load_system_fonts();
    let source = fs::read(root.join("assets/icon.svg")).context("无法读取 assets/icon.svg")?;
    let icon = usvg::Tree::from_data(&source, &options).context("无法解析应用图标 SVG")?;

    let png = render(&icon, 512, 512)?.encode_png()?;
    let ico = encode_ico(&icon)?;
    let preview = render_preview(&icon, &options)?.encode_png()?;

    fs::create_dir_all(root.join("docs"))?;
    for (name, data) in [
        ("assets/icon.png", png),
        ("assets/icon.ico", ico),
        ("docs/icon-preview.png", preview),
    ] {
        fs::write(root.join(name), data).with_context(|| format!("无法写入 {name}"))?;
        println!("Generated {name}");
    }
    Ok(())
}

fn render(tree: &usvg::Tree, width: u32, height: u32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(width, height).context("无法分配图标画布")?;
    let scale = Transform::from_scale(
        width as f32 / tree.size().width(),
        height as f32 / tree.size().height(),
    );
    resvg::render(tree, scale, &mut pixmap.as_mut());
    Ok(pixmap)
}

fn encode_ico(tree: &usvg::Tree) -> Result<Vec<u8>> {
    let frames = ICON_SIZES
        .iter()
        .map(|&size| Ok(render(tree, size, size)?.encode_png()?))
        .collect::<Result<Vec<_>>>()?;
    let mut ico = Vec::new();
    ico.write_all(&0_u16.to_le_bytes())?; // Reserved.
    ico.write_all(&1_u16.to_le_bytes())?; // Icon, rather than cursor.
    ico.write_all(&(ICON_SIZES.len() as u16).to_le_bytes())?;
    let mut offset = 6 + 16 * ICON_SIZES.len();

    // Windows Vista and later support PNG frames; 256px is encoded as zero.
    for (&size, frame) in ICON_SIZES.iter().zip(&frames) {
        let dimension = (size % 256) as u8;
        ico.write_all(&[dimension, dimension, 0, 0])?;
        ico.write_all(&1_u16.to_le_bytes())?; // Color planes.
        ico.write_all(&32_u16.to_le_bytes())?; // RGBA bits per pixel.
        ico.write_all(&u32::try_from(frame.len())?.to_le_bytes())?;
        ico.write_all(&u32::try_from(offset)?.to_le_bytes())?;
        offset += frame.len();
    }
    for frame in frames {
        ico.extend_from_slice(&frame);
    }
    Ok(ico)
}

fn render_preview(icon: &usvg::Tree, options: &usvg::Options<'_>) -> Result<Pixmap> {
    let panels = usvg::Tree::from_str(PREVIEW, options).context("无法解析预览画布 SVG")?;
    let mut preview = render(&panels, 960, 420)?;
    let paint = PixmapPaint::default();
    let transform = Transform::identity();
    let large = render(icon, 256, 256)?;
    preview.draw_pixmap(112, 96, large.as_ref(), &paint, transform, None);

    for (size, x) in [(48, 524), (32, 640), (24, 742), (16, 838)] {
        let sample = render(icon, size, size)?;
        for center_y in [120, 292] {
            preview.draw_pixmap(
                x,
                center_y - size as i32 / 2,
                sample.as_ref(),
                &paint,
                transform,
                None,
            );
        }
    }
    Ok(preview)
}
