//! Regenerate the saved icon assets with `cargo run --example generate-icon`.

use anyhow::{Context, Result};
use resvg::{
    tiny_skia::{Pixmap, PixmapPaint, Transform},
    usvg,
};
use std::{fs, io::Write, path::Path};

const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
// Leave room around the artwork to match the optical size of macOS Dock icons.
const MACOS_ICON_SCALE: f32 = 0.86;
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
    let macos_png = render_macos_icon(&icon, 512)?.encode_png()?;
    let ico = encode_ico(&icon)?;
    let icns = encode_icns(&icon)?;
    let preview = render_preview(&icon, &options)?.encode_png()?;

    fs::create_dir_all(root.join("docs"))?;
    for (name, data) in [
        ("assets/icon.png", png),
        ("assets/icon-macos.png", macos_png),
        ("assets/icon.ico", ico),
        ("assets/icon.icns", icns),
        ("docs/icon-preview.png", preview),
    ] {
        fs::write(root.join(name), data).with_context(|| format!("无法写入 {name}"))?;
        println!("Generated {name}");
    }
    Ok(())
}

fn render(tree: &usvg::Tree, width: u32, height: u32) -> Result<Pixmap> {
    render_scaled(tree, width, height, 1.0)
}

fn render_macos_icon(tree: &usvg::Tree, size: u32) -> Result<Pixmap> {
    render_scaled(tree, size, size, MACOS_ICON_SCALE)
}

fn render_scaled(tree: &usvg::Tree, width: u32, height: u32, artwork_scale: f32) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(width, height).context("无法分配图标画布")?;
    let scale = Transform::from_scale(
        width as f32 / tree.size().width() * artwork_scale,
        height as f32 / tree.size().height() * artwork_scale,
    )
    .post_translate(
        width as f32 * (1.0 - artwork_scale) / 2.0,
        height as f32 * (1.0 - artwork_scale) / 2.0,
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

fn encode_icns(tree: &usvg::Tree) -> Result<Vec<u8>> {
    // macOS PNG chunks: standard and Retina representations up to 1024px.
    let mut chunks = Vec::new();
    for (kind, size) in [
        (*b"icp4", 16),
        (*b"ic11", 32),
        (*b"icp5", 32),
        (*b"ic12", 64),
        (*b"ic07", 128),
        (*b"ic13", 256),
        (*b"ic08", 256),
        (*b"ic14", 512),
        (*b"ic09", 512),
        (*b"ic10", 1024),
    ] {
        let png = render_macos_icon(tree, size)?.encode_png()?;
        chunks.extend_from_slice(&kind);
        chunks.extend_from_slice(&u32::try_from(png.len() + 8)?.to_be_bytes());
        chunks.extend_from_slice(&png);
    }
    let mut icns = Vec::from(*b"icns");
    icns.extend_from_slice(&u32::try_from(chunks.len() + 8)?.to_be_bytes());
    icns.extend_from_slice(&chunks);
    Ok(icns)
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
