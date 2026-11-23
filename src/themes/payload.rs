//! DreamSkin-compatible payload assembly. Importing ZIPs never calls this module.

use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const RENDERER: &str = include_str!("../../assets/dreamskin/renderer-inject.js");
const BASE_CSS: &str = include_str!("../../assets/dreamskin/dream-skin.css");
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The caller must also unregister its Page.addScriptToEvaluateOnNewDocument scripts.
pub(super) const CLEANUP: &str = r#"(() => {
  window.__CODEX_DREAM_SKIN_DISABLED__ = true;
  window.__CODEX_DREAM_SKIN_EARLY_GENERATION__ = 'disabled';
  window.__CODEX_SWITCH_THEME_GENERATION__ = 'disabled';
  delete window.__CODEX_DREAM_SKIN_EARLY_APPLIED__;
  const state = window.__CODEX_DREAM_SKIN_STATE__;
  try { state?.cleanup?.(); } catch {}
  const root = document.documentElement;
  for (const attr of [...(root?.attributes || [])]) {
    if (attr.name.startsWith('data-dream-')) root.removeAttribute(attr.name);
  }
  for (const property of [...(root?.style || [])]) {
    if (property.startsWith('--dream-') || property.startsWith('--ds-')) root.style.removeProperty(property);
  }
  for (const node of document.querySelectorAll('[data-ds-part]')) node.removeAttribute('data-ds-part');
  const sheets = window.__CODEX_DREAM_SKIN_STYLE_SHEETS__;
  if (sheets && 'adoptedStyleSheets' in document) {
    document.adoptedStyleSheets = [...document.adoptedStyleSheets].filter(sheet => !sheets.has(sheet));
  }
  try { if (state?.artUrl) URL.revokeObjectURL(state.artUrl); } catch {}
  document.getElementById('codex-dream-skin-style')?.remove();
  delete window.__CODEX_DREAM_SKIN_STYLE_SHEETS__;
  delete window.__CODEX_DREAM_SKIN_STATE__;
  delete window.__CODEX_DREAM_SKIN_ANALYSIS_CACHE__;
  return true;
})()"#;

pub(super) fn theme_name(dir: &Path) -> String {
    let fallback = || {
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string())
    };
    read_theme(dir)
        .ok()
        .and_then(|raw| display_name(&raw).map(str::to_owned))
        .unwrap_or_else(fallback)
}

fn display_name(raw: &Value) -> Option<&str> {
    ["name", "displayName", "title"]
        .into_iter()
        .find_map(|key| raw[key].as_str().map(str::trim).filter(|s| !s.is_empty()))
}

fn read_theme(dir: &Path) -> Result<Value> {
    let path = dir.join("theme.json");
    let source = fs::read_to_string(&path).with_context(
        || crate::tr!("payload.could_not_read_the_theme_configuration"; arg0 = path.display()),
    )?;
    serde_json::from_str(source.trim_start_matches('\u{feff}')).with_context(
        || crate::tr!("payload.could_not_parse_the_theme_json"; arg0 = path.display()),
    )
}

pub(super) fn default_transparency(dir: &Path) -> u8 {
    let alpha = read_theme(dir).ok().and_then(|theme| {
        let color = theme["colors"]["panel"].as_str()?.trim();
        if let Some(hex) = color.strip_prefix('#') {
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            return match hex.len() {
                4 => u8::from_str_radix(&hex[3..4], 16)
                    .ok()
                    .map(|n| f64::from(n) / 15.),
                8 => u8::from_str_radix(&hex[6..8], 16)
                    .ok()
                    .map(|n| f64::from(n) / 255.),
                _ => None,
            };
        }
        let lower = color.to_ascii_lowercase();
        let body = lower
            .strip_prefix("rgba(")
            .or_else(|| lower.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        let components: Vec<_> = body.split(',').map(str::trim).collect();
        if components.len() != 4 {
            return None;
        }
        components[3]
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(|n| n.clamp(0., 1.))
    });
    alpha
        .map(|alpha| ((1. - alpha) * 100.).round() as u8)
        .unwrap_or(30)
}

pub(super) fn build(theme_dir: &Path, transparency: Option<u8>) -> Result<String> {
    let mut theme = read_theme(theme_dir)?;
    if !theme.is_object() {
        bail!(
            crate::tr!("payload.the_theme_configuration_root_must_be_a_json_object"; arg0 = theme_dir.display())
        );
    }
    let name = display_name(&theme)
        .map(str::to_owned)
        .unwrap_or_else(|| theme_name(theme_dir));
    let image = image_reference(&theme).with_context(
        || crate::tr!("payload.theme_has_no_background_image_the_image_field_in"; name = name),
    )?;
    let art_data = if image.starts_with("data:image/") {
        image.to_owned()
    } else {
        let path = theme_dir.join(image.replace('\\', "/"));
        let bytes = fs::read(&path)
            .with_context(|| crate::tr!("payload.could_not_read_the_background_image_for_theme"; name = name, arg0 = path.display()))?;
        if bytes.is_empty() {
            bail!(
                crate::tr!("payload.the_background_image_for_theme_is_empty"; name = name, arg0 = path.display())
            );
        }
        if let Some(metadata) = image_metadata(&bytes) {
            theme["artMetadata"] = metadata;
        }
        theme["artKey"] = Value::String(hash(&bytes));
        format!(
            "data:{};base64,{}",
            mime(&path, &bytes),
            STANDARD.encode(bytes)
        )
    };
    theme["name"] = Value::String(name.clone());
    if theme["id"].as_str().is_none_or(str::is_empty) {
        theme["id"] = Value::String(theme_dir.to_string_lossy().into_owned());
    }
    // Keep all author fields, including the legacy Windows palette. Only add
    // loader-derived information the renderer normally receives from DreamSkin.
    if let Some(colors) = theme["colors"].as_object() {
        let keys: Vec<_> = colors.keys().cloned().collect();
        theme["explicitColorKeys"] = json!(keys);
        theme["colorMode"] = json!("explicit");
    } else {
        theme["colorMode"] = json!("auto");
    }
    if let Some(art) = theme["art"].as_object_mut() {
        for key in ["focusX", "focusY"] {
            if let Some(value) = art
                .get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|value| value.is_finite())
            {
                art.insert(key.into(), json!(value));
            }
        }
    }
    theme.as_object_mut().unwrap().remove("userTransparency");
    if let Some(value) = transparency {
        if value > 100 {
            bail!(crate::tr!(
                "payload.theme_transparency_must_be_between_0_and_100"
            ));
        }
        theme["userTransparency"] = json!(value);
    }
    let css_path = theme_dir.join("theme.css");
    let css = match fs::read_to_string(&css_path) {
        Ok(source) => format!("{BASE_CSS}\n{}", compile_css(&source)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BASE_CSS.to_owned(),
        Err(error) => {
            return Err(error).with_context(
                || crate::tr!("payload.could_not_read_theme_styles"; arg0 = css_path.display()),
            );
        }
    };
    let generation = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        GENERATION.fetch_add(1, Ordering::Relaxed)
    );
    let replacements = [
        ("__DREAM_SKIN_CSS_JSON__", serde_json::to_string(&css)?),
        // Refer to the outer argument instead of duplicating image data in
        // the preflight decoder and renderer within every CDP payload.
        ("__DREAM_SKIN_ART_JSON__", "_codexSwitchArtData".to_owned()),
        ("__DREAM_SKIN_THEME_JSON__", serde_json::to_string(&theme)?),
        (
            "__DREAM_SKIN_VERSION_JSON__",
            json!("codex-switch/dreamskin-1.5.21").to_string(),
        ),
        (
            "__DREAM_SKIN_STYLE_REVISION_JSON__",
            serde_json::to_string(&hash(css.as_bytes()))?,
        ),
        (
            "__DREAM_SKIN_PAYLOAD_REVISION_JSON__",
            serde_json::to_string(&generation)?,
        ),
    ];
    // Scan the original template once: author strings containing placeholder
    // names or '$&' must never be interpreted as more template source.
    let mut payload = String::with_capacity(RENDERER.len() + css.len());
    let mut rest = RENDERER;
    while let Some(start) = rest.find("__DREAM_SKIN_") {
        payload.push_str(&rest[..start]);
        let Some((token, value)) = replacements
            .iter()
            .find(|(token, _)| rest[start..].starts_with(token))
        else {
            bail!(crate::tr!(
                "payload.the_built_in_theme_renderer_contains_an_unknown_template"
            ));
        };
        payload.push_str(value);
        rest = &rest[start + token.len()..];
    }
    payload.push_str(rest);
    // Decode failures must surface during Apply, instead of the upstream
    // renderer's silent fallback. CDP evaluates this with awaitPromise=true.
    Ok(format!(
        "(async (_codexSwitchArtData) => {{ const route = new URLSearchParams(location.search).get('initialRoute') || ''; if (location.protocol !== 'app:' || location.pathname.endsWith('/avatar-overlay-composition-surface.html') || route.startsWith('/avatar-overlay')) return {{installed: false, excluded: true}}; const generation = {}; window.__CODEX_SWITCH_THEME_GENERATION__ = generation; const picture = new Image(); picture.src = _codexSwitchArtData; try {{ await picture.decode(); }} catch (_) {{ throw new Error({}); }} if (window.__CODEX_SWITCH_THEME_GENERATION__ !== generation) return {{installed: false, cancelled: true}}; return ({payload}); }})({})",
        serde_json::to_string(&generation)?,
        serde_json::to_string(
            &crate::tr!("payload.could_not_decode_the_background_image_for_theme_check"; name = name)
        )?,
        serde_json::to_string(&art_data)?,
    ))
}

fn image_reference(theme: &Value) -> Option<&str> {
    [
        "image",
        "backgroundImage",
        "imagePath",
        "backgroundPath",
        "wallpaper",
    ]
    .into_iter()
    .find_map(|key| theme[key].as_str().filter(|s| !s.is_empty()))
    .or_else(|| theme["background"]["image"].as_str())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn mime(path: &Path, bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        return "image/jpeg";
    }
    if bytes.get(8..12) == Some(b"WEBP") {
        return "image/webp";
    }
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => "image/png",
    }
}

fn image_metadata(bytes: &[u8]) -> Option<Value> {
    let (width, height) = image_dimensions(bytes)?;
    if width == 0 || height == 0 {
        return None;
    }
    let ratio = f64::from(width) / f64::from(height);
    let aspect = if ratio >= 2.25 {
        "ultrawide"
    } else if ratio >= 1.45 {
        "wide"
    } else if ratio >= 1.08 {
        "landscape"
    } else if ratio >= 0.9 {
        "square"
    } else {
        "portrait"
    };
    Some(
        json!({"width":width,"height":height,"ratio":ratio,"wide":ratio>=1.75,
        "aspect":aspect,"taskMode":if ratio>=2.25 {"banner"} else {"ambient"}}),
    )
}

// These dimensions are an initial layout hint, not an import or media validator.
// Chromium performs the actual decode when Apply runs.
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        return Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        ));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut offset = 2;
        while offset + 3 < bytes.len() {
            if bytes[offset] != 0xff {
                offset += 1;
                continue;
            }
            while bytes.get(offset) == Some(&0xff) {
                offset += 1;
            }
            let marker = *bytes.get(offset)?;
            offset += 1;
            if marker == 0xd9 || marker == 0xda {
                break;
            }
            if marker == 1 || (0xd0..=0xd8).contains(&marker) {
                continue;
            }
            let length = usize::from(u16::from_be_bytes(
                bytes.get(offset..offset + 2)?.try_into().ok()?,
            ));
            if length < 2 || offset.checked_add(length)? > bytes.len() {
                break;
            }
            if [
                0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
            ]
            .contains(&marker)
                && length >= 7
            {
                let height = u32::from(u16::from_be_bytes(
                    bytes[offset + 3..offset + 5].try_into().ok()?,
                ));
                let width = u32::from(u16::from_be_bytes(
                    bytes[offset + 5..offset + 7].try_into().ok()?,
                ));
                return Some((width, height));
            }
            offset += length;
        }
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        let mut offset = 12usize;
        while offset.checked_add(8)? <= bytes.len() {
            let kind = &bytes[offset..offset + 4];
            let size = usize::try_from(u32::from_le_bytes(
                bytes[offset + 4..offset + 8].try_into().ok()?,
            ))
            .ok()?;
            let data = bytes.get(offset + 8..offset.checked_add(8)?.checked_add(size)?)?;
            if kind == b"VP8X" && data.len() >= 10 {
                let little24 = |slice: &[u8]| {
                    u32::from(slice[0]) | (u32::from(slice[1]) << 8) | (u32::from(slice[2]) << 16)
                };
                return Some((little24(&data[4..7]) + 1, little24(&data[7..10]) + 1));
            }
            if kind == b"VP8L" && data.len() >= 5 && data[0] == 0x2f {
                let width = 1 + u32::from(data[1]) + (u32::from(data[2] & 0x3f) << 8);
                let height = 1
                    + u32::from(data[2] >> 6)
                    + (u32::from(data[3]) << 2)
                    + (u32::from(data[4] & 0xf) << 10);
                return Some((width, height));
            }
            if kind == b"VP8 " && data.len() >= 10 && data[3..6] == [0x9d, 0x01, 0x2a] {
                return Some((
                    u32::from(u16::from_le_bytes(data[6..8].try_into().ok()?) & 0x3fff),
                    u32::from(u16::from_le_bytes(data[8..10].try_into().ok()?) & 0x3fff),
                ));
            }
            offset = offset
                .checked_add(8)?
                .checked_add(size)?
                .checked_add(size % 2)?;
        }
    }
    None
}

#[derive(Clone)]
struct Rule {
    selector: String,
    declarations: Vec<(String, String)>,
}

/// Transform author CSS to DreamSkin's rendering layer, without its selector,
/// property, version, package-size or Safe CSS acceptance restrictions.
fn compile_css(source: &str) -> Result<String> {
    let source = without_comments(source);
    Ok(format!(
        "@layer dreamskin-community {{\n{}\n}}\n",
        compile_rules(&source)?
    ))
}

fn without_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut start = 0;
    let mut index = 0;
    let mut quote = None;
    while index < bytes.len() {
        if let Some(q) = quote {
            if bytes[index] == b'\\' {
                index = (index + 2).min(bytes.len());
                continue;
            }
            if bytes[index] == q {
                quote = None;
            }
        } else if bytes[index] == b'\'' || bytes[index] == b'"' {
            quote = Some(bytes[index]);
        } else if bytes[index..].starts_with(b"/*") {
            output.push_str(&source[start..index]);
            index += 2;
            while index + 1 < bytes.len() && !bytes[index..].starts_with(b"*/") {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
            output.push(' ');
            start = index;
            continue;
        }
        index += 1;
    }
    output.push_str(&source[start..]);
    output
}

/// Find structural delimiters outside strings, parentheses and square brackets.
fn delimiters(source: &str, wanted: &[u8]) -> Vec<(usize, u8)> {
    let bytes = source.as_bytes();
    let mut output = Vec::new();
    let (mut quote, mut parens, mut brackets) = (None, 0usize, 0usize);
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(q) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == q {
                quote = None;
            }
        } else if byte == b'\\' {
            index += 2;
            continue;
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else {
            if byte == b'(' {
                parens += 1;
            }
            if byte == b')' {
                parens = parens.saturating_sub(1);
            }
            if byte == b'[' {
                brackets += 1;
            }
            if byte == b']' {
                brackets = brackets.saturating_sub(1);
            }
            if parens == 0 && brackets == 0 && wanted.contains(&byte) {
                output.push((index, byte));
            }
        }
        index += 1;
    }
    output
}

fn compile_rules(source: &str) -> Result<String> {
    let structural = delimiters(source, b"{};");
    let mut index = 0;
    let mut start = 0;
    let mut rules = Vec::new();
    let mut chunks = Vec::new();
    let mut ordinal = 0;
    while index < structural.len() {
        let (open, token) = structural[index];
        if token == b';' {
            chunks.push((ordinal, source[start..=open].to_owned()));
            ordinal += 1;
            start = open + 1;
            index += 1;
            continue;
        }
        if token != b'{' {
            bail!(crate::tr!("payload.theme_css_has_unmatched_braces"));
        }
        let header = source[start..open].trim();
        let mut depth = 1;
        let mut close_index = index + 1;
        while close_index < structural.len() {
            match structural[close_index].1 {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            close_index += 1;
        }
        if depth != 0 {
            bail!(crate::tr!("payload.theme_css_has_unmatched_braces_2"; header = header));
        }
        let close = structural[close_index].0;
        let body = &source[open + 1..close];
        if header.starts_with('@') {
            let keyword = header
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if [
                "@media",
                "@supports",
                "@container",
                "@layer",
                "@scope",
                "@starting-style",
            ]
            .contains(&keyword.as_str())
            {
                chunks.push((
                    ordinal,
                    format!("{header} {{\n{}\n}}", compile_rules(body)?),
                ));
            } else {
                chunks.push((ordinal, format!("{header} {{{body}}}")));
            }
        } else {
            rules.push((
                ordinal,
                Rule {
                    selector: header.to_owned(),
                    declarations: declarations(body),
                },
            ));
        }
        start = close + 1;
        index = close_index + 1;
        ordinal += 1;
    }
    if !source[start..].trim().is_empty() {
        chunks.push((ordinal, source[start..].to_owned()));
    }
    let composer_borders: Vec<_> = rules
        .iter()
        .filter(|(_, rule)| part(&rule.selector) == Some(("composer", false)))
        .flat_map(|(_, rule)| rule.declarations.iter().map(|(property, _)| property))
        .filter(|property| is_border_bridge(property))
        .cloned()
        .collect();
    for (position, rule) in rules {
        let mut output = String::new();
        let selector = &rule.selector;
        let descriptor = part(selector);
        let mut compiled = rule.declarations.clone();
        for (property, value) in &rule.declarations {
            if property == "background-color"
                && descriptor.is_some_and(|(p, _)| ["sidebar", "main", "home"].contains(&p))
            {
                compiled.push(("background-image".into(), "none".into()));
            }
            if descriptor.is_some_and(|(p, _)| p == "composer")
                && composer_borders.contains(property)
            {
                compiled.push((format!("--ds-community-composer-{property}"), value.clone()));
            }
        }
        emit_rule(&mut output, selector, &compiled);
        if descriptor.is_some_and(|(p, _)| p == "home-hero") {
            let sizes: Vec<_> = rule
                .declarations
                .iter()
                .filter(|(p, _)| p == "font-size")
                .cloned()
                .collect();
            if !sizes.is_empty() {
                emit_rule(&mut output, &format!("{selector}::after"), &sizes);
            }
        }
        if descriptor.is_some_and(|(p, _)| p == "root") {
            let bridge: Vec<_> = rule
                .declarations
                .iter()
                .filter(|(p, _)| {
                    [
                        "background-color",
                        "color",
                        "font-family",
                        "font-size",
                        "font-weight",
                        "letter-spacing",
                        "line-height",
                    ]
                    .contains(&p.as_str())
                })
                .cloned()
                .collect();
            if !bridge.is_empty() {
                emit_rule(&mut output, &format!("{selector} body"), &bridge);
            }
        }
        if descriptor.is_some_and(|(p, _)| p == "composer-toolbar") {
            let colors: Vec<_> = rule
                .declarations
                .iter()
                .filter(|(p, _)| p == "color")
                .cloned()
                .collect();
            if !colors.is_empty() {
                emit_rule(
                    &mut output,
                    &format!(
                        "{selector} :where(button:not([class~=\"bg-token-foreground\"]), button:not([class~=\"bg-token-foreground\"]) *)"
                    ),
                    &colors,
                );
            }
        }
        chunks.push((position, output));
    }
    chunks.sort_by_key(|(position, _)| *position);
    Ok(chunks
        .into_iter()
        .map(|(_, source)| source)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn declarations(body: &str) -> Vec<(String, String)> {
    let mut output = Vec::new();
    let mut start = 0;
    let mut ends: Vec<_> = delimiters(body, b";").into_iter().map(|(i, _)| i).collect();
    ends.push(body.len());
    for end in ends {
        let declaration = body[start..end].trim();
        if let Some((colon, _)) = delimiters(declaration, b":").first() {
            let property = declaration[..*colon].trim();
            let value = declaration[*colon + 1..].trim();
            let value = if value.to_ascii_lowercase().ends_with("!important") {
                value[..value.len() - 10].trim_end()
            } else {
                value
            };
            if !property.is_empty() && !value.is_empty() {
                let property = if property.starts_with("--") {
                    property.to_owned()
                } else {
                    property.to_ascii_lowercase()
                };
                output.push((property, value.to_owned()));
            }
        }
        start = end + 1;
    }
    output
}

fn part(selector: &str) -> Option<(&str, bool)> {
    let value = selector.strip_prefix("[data-ds-part=")?;
    let quote = value.as_bytes().first().copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let end = value[1..].find(quote as char)? + 1;
    let suffix = value.get(end + 1..)?.strip_prefix(']')?;
    if !suffix.is_empty() && !suffix.starts_with(':') {
        return None;
    }
    Some((&value[1..end], !suffix.is_empty()))
}

fn is_border_bridge(property: &str) -> bool {
    property.starts_with("border-")
        && ["color", "width", "style"]
            .iter()
            .any(|ending| property.ends_with(ending))
}

fn emit_rule(output: &mut String, selector: &str, declarations: &[(String, String)]) {
    output.push_str(&format!("\n  {selector} {{\n"));
    for (property, value) in declarations {
        output.push_str(&format!("    {property}: {value} !important;\n"));
    }
    output.push_str("  }\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("art.png"), b"test-image-bytes").unwrap();
        fs::write(dir.path().join("theme.json"), r##"{"name":"作者$&__DREAM_SKIN_ART_JSON__","image":"art.png","schemaVersion":"legacy","palette":{"accent":"#f00"},"colors":{"panel":"rgba(1, 2, 3, .25)"},"extra":{"preserved":true},"userTransparency":90}"##).unwrap();
        dir
    }

    #[test]
    fn payload_preserves_author_fields_and_ignores_manifest_versions() {
        let dir = fixture();
        fs::write(dir.path().join("manifest.json"), "not even JSON").unwrap();
        let payload = build(dir.path(), None).unwrap();
        assert!(payload.contains(r#""schemaVersion":"legacy""#));
        assert!(payload.contains(r##""palette":{"accent":"#f00"}"##));
        assert!(payload.contains(r#""panel":"rgba(1, 2, 3, .25)""#));
        assert!(payload.contains(r#""extra":{"preserved":true}"#));
        assert!(!payload.contains(r#""userTransparency":90"#));
        assert!(payload.contains("作者$&__DREAM_SKIN_ART_JSON__"));
        assert!(payload.contains("await picture.decode()"));
        assert_eq!(payload.matches("data:image/png;base64,").count(), 1);
    }

    #[test]
    fn payload_transparency_and_generation_change_without_touching_author_files() {
        let dir = fixture();
        let before = fs::read(dir.path().join("theme.json")).unwrap();
        let first = build(dir.path(), Some(42)).unwrap();
        let second = build(dir.path(), Some(42)).unwrap();
        assert!(first.contains(r#""userTransparency":42"#));
        assert_ne!(first, second);
        assert_eq!(before, fs::read(dir.path().join("theme.json")).unwrap());
    }

    // Guard the actual surface declarations, not just the presence of alpha
    // variables: a fixed transparent background leaves the slider ineffective.
    fn wide_surface_rule(selector: &str, mode: Option<&str>) -> String {
        let css = without_comments(BASE_CSS);
        css.split('}')
            .filter_map(|block| block.split_once('{'))
            .find(|(header, _)| {
                header.contains("[data-dream-art-wide=\"true\"]")
                    && header.trim_end().ends_with(selector)
                    && mode.is_none_or(|mode| {
                        header.contains(&format!("[data-dream-task-mode=\"{mode}\"]"))
                    })
            })
            .unwrap_or_else(|| panic!("missing wide surface rule: {selector}, {mode:?}"))
            .1
            .to_owned()
    }

    fn assert_scrim(body: &str, prefix: &str) {
        let background = declarations(body)
            .into_iter()
            .find(|(property, _)| property == "background")
            .expect("surface must have a background")
            .1;
        assert!(
            background.starts_with("linear-gradient(90deg,"),
            "{background}"
        );
        for stop in ["edge", "mid", "far"] {
            assert!(background.contains(&format!("var(--ds-{prefix}{stop})")));
        }
    }

    #[test]
    fn wide_native_threads_keep_the_adjustable_scrim_in_all_task_modes() {
        let selector = "main:is(.main-surface, [data-app-shell-main-surface], [class*=\"_MainContentSurface_\"]):not(:has([role=\"main\"]))";
        for mode in ["ambient", "banner"] {
            assert_scrim(&wide_surface_rule(selector, Some(mode)), "task-immersive-");
        }
        assert_scrim(&wide_surface_rule(selector, Some("full")), "immersive-");
    }

    #[test]
    fn wide_legacy_surfaces_keep_the_adjustable_scrim() {
        assert_scrim(
            &wide_surface_rule(
                ":not(:has([data-ds-part=\"main\"])) > [data-ds-part=\"home\"]",
                None,
            ),
            "immersive-",
        );
    }

    #[test]
    fn wide_artwork_still_clears_the_macos_window_backdrop() {
        let body = wide_surface_rule(".macos-window-backdrop", None);
        assert!(declarations(&body).contains(&("background".into(), "transparent".into())));
    }

    #[test]
    fn broken_theme_can_be_named_but_fails_when_applied() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("theme.json"), "bad JSON").unwrap();
        assert!(!theme_name(dir.path()).is_empty());
        assert!(
            build(dir.path(), None)
                .unwrap_err()
                .to_string()
                .contains("JSON")
        );
        fs::write(
            dir.path().join("theme.json"),
            r#"{"displayName":"旧主题","image":"missing.jpg"}"#,
        )
        .unwrap();
        assert_eq!(theme_name(dir.path()), "旧主题");
        assert!(
            build(dir.path(), None)
                .unwrap_err()
                .to_string()
                .contains(&crate::tr!("payload.could_not_read_the_background_image_for_theme"; name = "旧主题", arg0 = dir.path().join("missing.jpg").display()))
        );
    }

    #[test]
    fn css_matches_background_and_composer_bridges_without_safe_css_restrictions() {
        let css = compile_css(
            r#"
            /* Author comments are accepted. */
            [data-ds-part="sidebar"] { background-color: rgba(1, 2, 3, .25); }
            [data-ds-part="composer"] { border-color: red; }
            [data-ds-part="composer"]:hover { border-color: blue; }
            [data-ds-part="home-hero"] { font-size: 18px; }
            [data-ds-part="root"] { color: red; }
            [data-ds-part="composer-toolbar"] { color: blue; }
            .custom > a { transform: translateX(2px); content: "a;b:c/*d*/"; }
        "#,
        )
        .unwrap();
        assert!(css.starts_with("@layer dreamskin-community"));
        assert!(css.contains("background-image: none !important;"));
        assert!(css.contains("--ds-community-composer-border-color: blue !important;"));
        assert!(css.contains(r#"[data-ds-part="home-hero"]::after"#));
        assert!(css.contains(r#"[data-ds-part="root"] body"#));
        assert!(css.contains("button:not([class~="));
        assert!(css.contains("transform: translateX(2px) !important;"));
        assert!(css.contains(r#"content: "a;b:c/*d*/" !important;"#));
    }

    #[test]
    fn css_handles_media_queries_and_existing_important_once() {
        let css = compile_css(r#"@media (width > 500px) { [data-ds-part="main"] { background-color: #fff !IMPORTANT; } }"#).unwrap();
        assert!(css.contains("@media (width > 500px)"));
        assert!(css.contains("background-color: #fff !important;"));
        assert!(!css.contains("!IMPORTANT !important"));
        assert!(css.contains("background-image: none !important;"));
    }

    #[test]
    fn css_preserves_author_cascade_order_and_escaped_selector_tokens() {
        let css = compile_css(r".first { color: red; } @media (width > 500px) { .middle { color: blue; } } .escaped\{name { color: green; }").unwrap();
        assert!(css.find(".first").unwrap() < css.find("@media").unwrap());
        assert!(css.find("@media").unwrap() < css.find(r".escaped\{name").unwrap());
        assert!(css.contains("color: green !important;"));
    }

    #[test]
    fn jpeg_and_webp_get_the_same_initial_aspect_classification_as_dreamskin() {
        // Minimal headers sufficient for dimensions; actual decoding remains
        // Chromium's job at Apply time.
        let jpeg = [0xff, 0xd8, 0xff, 0xc0, 0, 7, 8, 0x05, 0xa0, 0x0a, 0x00];
        let metadata = image_metadata(&jpeg).unwrap();
        assert_eq!(metadata["width"], 2560);
        assert_eq!(metadata["height"], 1440);
        assert_eq!(metadata["aspect"], "wide");
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        webp.extend([0xff, 0x09, 0, 0x9f, 0x05, 0]);
        assert_eq!(image_metadata(&webp).unwrap()["width"], 2560);
        assert_eq!(image_metadata(&webp).unwrap()["height"], 1440);
    }
}
