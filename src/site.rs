//! Guest-facing website content: editable pages (Markdown) and the photo gallery.
//!
//! Pages and photo metadata live in SQLite; photo files live in `<data>/media`
//! (resized and re-encoded on upload, which strips EXIF/GPS) and are copied to
//! the backup bucket by the daily backup.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::db::now;
use crate::store::audit;

/// Pages every site has. They can be edited and unpublished but not deleted.
pub const BUILTIN: &[&str] = &["schedule", "gallery"];

/// Slugs that would collide with other routes.
const RESERVED: &[&str] = &["admin", "i", "static", "fonts", "media", "healthz", "robots.txt", "favicon.ico"];

pub const AUDIENCES: &[(&str, &str)] = &[("all", "Everyone"), ("australia", "Households in Australia"), ("us", "Households outside Australia")];

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub slug: String,
    pub title: String,
    pub body: String,
    pub audience: String,
    pub published: bool,
    pub sort: i64,
    pub updated_at: String,
    pub updated_by: String,
}

impl Page {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            slug: r.get("slug")?,
            title: r.get("title")?,
            body: r.get("body")?,
            audience: r.get("audience")?,
            published: r.get("published")?,
            sort: r.get("sort")?,
            updated_at: r.get("updated_at")?,
            updated_by: r.get("updated_by")?,
        })
    }
    pub fn shown_to(&self, australia: bool) -> bool {
        match self.audience.as_str() {
            "australia" => australia,
            "us" => !australia,
            _ => true,
        }
    }
    pub fn builtin(&self) -> bool {
        BUILTIN.contains(&self.slug.as_str())
    }
}

pub fn valid_slug(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !RESERVED.contains(&s)
}

pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

pub fn pages(c: &Connection) -> Result<Vec<Page>> {
    let mut st = c.prepare("SELECT * FROM pages ORDER BY sort, title")?;
    Ok(st.query_map([], Page::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn get_page(c: &Connection, slug: &str) -> Result<Option<Page>> {
    Ok(c.query_row("SELECT * FROM pages WHERE slug = ?1", [slug], Page::from_row).optional()?)
}

pub struct PageInput {
    pub title: String,
    pub body: String,
    pub audience: String,
    pub published: bool,
    pub sort: i64,
}

pub fn save_page(c: &Connection, actor: &str, slug: &str, p: &PageInput) -> Result<()> {
    if !valid_slug(slug) {
        bail!("page addresses use lowercase letters, numbers and dashes");
    }
    if p.title.trim().is_empty() || p.title.len() > 100 {
        bail!("give the page a title (up to 100 characters)");
    }
    if p.body.len() > 50_000 {
        bail!("that page is too long");
    }
    if !AUDIENCES.iter().any(|(a, _)| *a == p.audience) {
        bail!("unknown audience");
    }
    let tx = crate::db::begin(c)?;
    tx.execute(
        "INSERT INTO pages (slug, title, body, audience, published, sort, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(slug) DO UPDATE SET title = excluded.title, body = excluded.body, audience = excluded.audience,
         published = excluded.published, sort = excluded.sort, updated_at = excluded.updated_at, updated_by = excluded.updated_by",
        params![slug, p.title.trim(), p.body, p.audience, p.published, p.sort, now(), actor],
    )?;
    audit(&tx, actor, "page.save", "page", slug, &json!({"title": p.title.trim(), "published": p.published, "audience": p.audience}))?;
    tx.commit()?;
    Ok(())
}

pub fn delete_page(c: &Connection, actor: &str, slug: &str) -> Result<()> {
    if BUILTIN.contains(&slug) {
        bail!("the {slug} page can be unpublished but not deleted");
    }
    let tx = crate::db::begin(c)?;
    tx.execute("DELETE FROM pages WHERE slug = ?1", [slug])?;
    audit(&tx, actor, "page.delete", "page", slug, &json!({}))?;
    tx.commit()?;
    Ok(())
}

/// First run: create the built-in pages and turn the config's travel info into pages.
pub fn seed(c: &Connection, cfg: &Config) -> Result<()> {
    let n: i64 = c.query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    let section = |audience: &str| -> String {
        cfg.wedding
            .info
            .iter()
            .filter(|s| s.audience.as_deref().unwrap_or("all") == audience)
            .map(|s| format!("## {}\n\n{}\n", s.title, s.body))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut travel = section("all");
    let au = section("australia");
    if !au.is_empty() {
        travel.push_str(&format!("\n::: australia\n{au}:::\n"));
    }
    let seeds = vec![
        ("schedule", "Schedule", String::new(), "all", true, 10),
        ("travel", "Travel", travel, "all", true, 20),
        ("gallery", "Gallery", String::new(), "all", false, 90),
    ];
    for (slug, title, body, audience, published, sort) in seeds {
        c.execute(
            "INSERT OR IGNORE INTO pages (slug, title, body, audience, published, sort, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'system')",
            params![slug, title, body, audience, published, sort, now()],
        )?;
    }
    Ok(())
}

/// Markdown to HTML. Raw HTML in the source is shown as text, never rendered.
/// Keep only the parts of a page meant for this household. Text between a line `::: australia`
/// and a line `:::` shows only to households in Australia; `::: us` only to everyone else.
pub fn for_household(src: &str, australia: bool) -> String {
    let mut out = String::new();
    let mut show = true;
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(":::") {
            show = match rest.trim() {
                "australia" => australia,
                "us" => !australia,
                _ => true, // a bare `:::` ends the section
            };
            continue;
        }
        if show {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub fn markdown(src: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};
    let parser = Parser::new_ext(src, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH).map(|ev| match ev {
        Event::Html(t) | Event::InlineHtml(t) => Event::Text(t),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

// ---------------------------------------------------------------------------
// Photos

#[derive(Debug, Clone, Serialize)]
pub struct Photo {
    pub id: i64,
    pub file: String,
    pub caption: String,
    pub width: i64,
    pub height: i64,
    pub sort: i64,
    pub uploaded_by: String,
    pub created_at: String,
}

impl Photo {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            file: r.get("file")?,
            caption: r.get("caption")?,
            width: r.get("width")?,
            height: r.get("height")?,
            sort: r.get("sort")?,
            uploaded_by: r.get("uploaded_by")?,
            created_at: r.get("created_at")?,
        })
    }
    pub fn large(&self) -> String {
        format!("{}-l.jpg", self.file)
    }
    pub fn thumb(&self) -> String {
        format!("{}-t.jpg", self.file)
    }
}

pub fn photos(c: &Connection) -> Result<Vec<Photo>> {
    let mut st = c.prepare("SELECT * FROM photos WHERE deleted_at IS NULL ORDER BY sort, id")?;
    Ok(st.query_map([], Photo::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn media_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("media")
}

/// Only files we generated: `<uuid>-t.jpg` / `<uuid>-l.jpg`.
pub fn valid_media_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix("-t.jpg").or_else(|| name.strip_suffix("-l.jpg")) else { return false };
    stem.len() == 36 && stem.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

static DECODE: Mutex<()> = Mutex::new(());

/// Decode an uploaded photo, apply its EXIF orientation, and write a large
/// (max 2000px) and thumbnail (max 800px) JPEG. Re-encoding drops all metadata.
/// Returns (file stem, width, height) of the large version.
pub fn process_photo(data_dir: &Path, bytes: &[u8]) -> Result<(String, u32, u32)> {
    use image::{DynamicImage, ImageDecoder, ImageReader, codecs::jpeg::JpegEncoder, imageops::FilterType};
    // One at a time: decoding a 12MP photo needs ~50MB and the server is small.
    let _g = DECODE.lock().unwrap_or_else(|p| p.into_inner());
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(300 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| anyhow!("not a photo we can read ({e}); use JPEG, PNG or WebP"))?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);

    let dir = media_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let stem = uuid::Uuid::new_v4().to_string();
    let encode = |im: &DynamicImage, suffix: &str, quality: u8| -> Result<()> {
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, quality).encode_image(&im.to_rgb8())?;
        let path = dir.join(format!("{stem}-{suffix}.jpg"));
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &buf)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    };
    let large = if img.width().max(img.height()) > 2000 { img.resize(2000, 2000, FilterType::Triangle) } else { img.clone() };
    encode(&large, "l", 84)?;
    encode(&img.thumbnail(800, 800), "t", 80)?;
    Ok((stem, large.width(), large.height()))
}

pub fn add_photo(c: &Connection, actor: &str, file: &str, w: u32, h: u32, caption: &str) -> Result<i64> {
    let tx = crate::db::begin(c)?;
    let sort: i64 = tx.query_row("SELECT COALESCE(MAX(sort), 0) + 10 FROM photos", [], |r| r.get(0))?;
    tx.execute(
        "INSERT INTO photos (file, caption, width, height, sort, uploaded_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![file, caption.trim(), w, h, sort, actor, now()],
    )?;
    let id = tx.last_insert_rowid();
    audit(&tx, actor, "photo.add", "photo", &id.to_string(), &json!({}))?;
    tx.commit()?;
    Ok(id)
}

pub fn set_caption(c: &Connection, actor: &str, id: i64, caption: &str) -> Result<()> {
    if caption.len() > 1000 {
        bail!("caption too long");
    }
    let n = c.execute("UPDATE photos SET caption = ?1 WHERE id = ?2 AND deleted_at IS NULL", params![caption.trim(), id])?;
    if n == 0 {
        bail!("photo not found");
    }
    audit(c, actor, "photo.caption", "photo", &id.to_string(), &json!({}))?;
    Ok(())
}

/// Swap a photo with its neighbour (`dir` = -1 earlier, +1 later).
pub fn move_photo(c: &Connection, actor: &str, id: i64, dir: i64) -> Result<()> {
    let list = photos(c)?;
    let i = list.iter().position(|p| p.id == id).context("photo not found")?;
    let j = i as i64 + dir;
    if j < 0 || j >= list.len() as i64 {
        return Ok(());
    }
    let (a, b) = (&list[i], &list[j as usize]);
    let tx = crate::db::begin(c)?;
    // Re-number so equal sort values can't get stuck.
    for (n, p) in list.iter().enumerate() {
        let pos = if p.id == a.id { j as usize } else if p.id == b.id { i } else { n };
        tx.execute("UPDATE photos SET sort = ?1 WHERE id = ?2", params![(pos as i64 + 1) * 10, p.id])?;
    }
    audit(&tx, actor, "photo.move", "photo", &id.to_string(), &json!({"dir": dir}))?;
    tx.commit()?;
    Ok(())
}

/// Hide a photo from the site. The files stay on disk (and in backups) so this can be undone by hand.
pub fn delete_photo(c: &Connection, actor: &str, id: i64) -> Result<()> {
    let n = c.execute("UPDATE photos SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL", params![now(), id])?;
    if n == 0 {
        bail!("photo not found");
    }
    audit(c, actor, "photo.delete", "photo", &id.to_string(), &json!({}))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_never_renders_html() {
        let out = markdown("## Hi\n\n<script>alert(1)</script>\n\nInline <img src=x onerror=alert(1)> and **bold** [link](https://example.com)");
        assert!(out.contains("<h2>Hi</h2>"));
        assert!(!out.contains("<script>") && !out.contains("<img"), "{out}");
        assert!(out.contains("&lt;script&gt;") && out.contains("&lt;img"));
        assert!(out.contains("<strong>bold</strong>"));
        assert!(out.contains(r#"<a href="https://example.com">link</a>"#));
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Things to do!"), "things-to-do");
        assert!(valid_slug("travel"));
        assert!(!valid_slug("admin"));
        assert!(!valid_slug("Travel"));
        assert!(valid_media_name("0b7e0b5e-1c1a-4c5e-9a58-1b1f2d0d2c11-t.jpg"));
        assert!(!valid_media_name("../../etc/passwd"));
    }

    #[test]
    fn photos_strip_metadata_and_resize() {
        let dir = std::env::temp_dir().join(format!("rsvp-media-{}", uuid::Uuid::new_v4()));
        let img = image::RgbImage::from_fn(3000, 1500, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let (stem, w, h) = process_photo(&dir, &png).unwrap();
        assert_eq!((w, h), (2000, 1000));
        assert!(media_dir(&dir).join(format!("{stem}-l.jpg")).exists());
        assert!(media_dir(&dir).join(format!("{stem}-t.jpg")).exists());
        assert!(process_photo(&dir, b"not an image").is_err());
    }
}

#[cfg(test)]
mod section_tests {
    #[test]
    fn australia_sections() {
        let body = "Everyone\n::: australia\nAU only\n:::\n::: us\nUS only\n:::\nEveryone again\n";
        assert_eq!(super::for_household(body, true), "Everyone\nAU only\nEveryone again\n");
        assert_eq!(super::for_household(body, false), "Everyone\nUS only\nEveryone again\n");
    }
}
