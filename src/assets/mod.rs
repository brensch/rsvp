//! Stationery generation: Typst templates + guest data -> PDF / PNG / SVG.
//!
//! Templates live in `templates/*.typ` and are compiled into the binary. If
//! `<data_dir>/templates/<name>.typ` exists it is used instead, so designs can
//! be tweaked on the server without a rebuild.

pub mod words;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

use crate::config::{Config, Env};
use crate::store::{Guest, Household, Response};

static FONTS: &[&[u8]] = &[
    include_bytes!("../../fonts/CormorantGaramond.ttf"),
    include_bytes!("../../fonts/CormorantGaramond-Italic.ttf"),
    include_bytes!("../../fonts/EBGaramond.ttf"),
    include_bytes!("../../fonts/EBGaramond-Italic.ttf"),
    include_bytes!("../../fonts/PinyonScript.ttf"),
];

pub static WEB_FONTS: &[(&str, &[u8])] = &[
    ("CormorantGaramond.ttf", include_bytes!("../../fonts/CormorantGaramond.ttf")),
    ("CormorantGaramond-Italic.ttf", include_bytes!("../../fonts/CormorantGaramond-Italic.ttf")),
    ("EBGaramond.ttf", include_bytes!("../../fonts/EBGaramond.ttf")),
    ("EBGaramond-Italic.ttf", include_bytes!("../../fonts/EBGaramond-Italic.ttf")),
    ("PinyonScript.ttf", include_bytes!("../../fonts/PinyonScript.ttf")),
];

static TEMPLATES: &[(&str, &str)] = &[
    ("lib", include_str!("../../templates/lib.typ")),
    ("save-the-date", include_str!("../../templates/save-the-date.typ")),
    ("invitation", include_str!("../../templates/invitation.typ")),
    ("envelope", include_str!("../../templates/envelope.typ")),
    ("labels", include_str!("../../templates/labels.typ")),
    ("place-cards", include_str!("../../templates/place-cards.typ")),
    ("report", include_str!("../../templates/report.typ")),
    ("monogram", include_str!("../../templates/monogram.typ")),
];

/// The printable things we can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    SaveTheDate,
    Invitation,
    Envelope,
    Labels,
    PlaceCards,
    Report,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::SaveTheDate, Kind::Invitation, Kind::Envelope, Kind::Labels, Kind::PlaceCards, Kind::Report];

    pub fn slug(self) -> &'static str {
        match self {
            Kind::SaveTheDate => "save-the-date",
            Kind::Invitation => "invitation",
            Kind::Envelope => "envelope",
            Kind::Labels => "labels",
            Kind::PlaceCards => "place-cards",
            Kind::Report => "report",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.slug() == s)
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::SaveTheDate => "Save the date",
            Kind::Invitation => "Invitation + details card",
            Kind::Envelope => "Envelopes (A7, print direct)",
            Kind::Labels => "Address labels",
            Kind::PlaceCards => "Place cards (attending guests)",
            Kind::Report => "RSVP report",
        }
    }
    /// Whether the kind supports the home-print "sheet" layout.
    pub fn has_sheet_layout(self) -> bool {
        matches!(self, Kind::SaveTheDate | Kind::Invitation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One card per page at trim size + 3mm bleed (send to a print shop).
    Card,
    /// 2-up on A4/Letter with crop marks (print at home, cut out).
    Sheet,
}

impl Layout {
    pub fn parse(s: &str) -> Self {
        if s == "sheet" { Layout::Sheet } else { Layout::Card }
    }
    fn as_str(self) -> &'static str {
        match self {
            Layout::Card => "card",
            Layout::Sheet => "sheet",
        }
    }
}

/// Everything about a household a template might need.
#[derive(Clone)]
pub struct Item<'a> {
    pub household: &'a Household,
    pub guests: &'a [Guest],
    pub responses: &'a [Response],
    pub token: String,
}

pub struct Renderer {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    template_dir: Option<PathBuf>,
}

impl Renderer {
    pub fn new(data_dir: &Path) -> Self {
        let fonts: Vec<Font> = FONTS.iter().flat_map(|b| Font::iter(Bytes::new(b.to_vec()))).collect();
        let book = FontBook::from_fonts(&fonts);
        let dir = data_dir.join("templates");
        Self {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(book),
            fonts,
            template_dir: dir.is_dir().then_some(dir),
        }
    }

    fn template_source(&self, name: &str) -> Result<String> {
        if let Some(dir) = &self.template_dir {
            let p = dir.join(format!("{name}.typ"));
            if p.is_file() {
                return std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()));
            }
        }
        TEMPLATES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| s.to_string())
            .ok_or_else(|| anyhow!("no template {name}"))
    }

    /// Compile a template with the given data and extra virtual files.
    pub fn compile(&self, kind: Kind, data: &Value, extra: HashMap<String, Vec<u8>>) -> Result<PagedDocument> {
        self.compile_named(kind.slug(), data, extra)
    }

    /// The wreath monogram as SVG for the website.
    pub fn monogram_svg(&self, cfg: &Config) -> Result<String> {
        let doc = self.compile_named("monogram", &base_data(cfg, Layout::Card)?, HashMap::new())?;
        to_svg(&doc, 0)
    }

    fn compile_named(&self, name: &str, data: &Value, extra: HashMap<String, Vec<u8>>) -> Result<PagedDocument> {
        let mut files: HashMap<FileId, Bytes> = HashMap::new();
        let main = file_id("/main.typ");
        files.insert(main, Bytes::from_string(self.template_source(name)?));
        files.insert(file_id("/lib.typ"), Bytes::from_string(self.template_source("lib")?));
        files.insert(file_id("/data.json"), Bytes::new(serde_json::to_vec(data)?));
        for (path, bytes) in extra {
            files.insert(file_id(&path), Bytes::new(bytes));
        }
        let world = DocWorld { r: self, main, files };
        let result = typst::compile::<PagedDocument>(&world);
        typst::comemo::evict(20);
        match result.output {
            Ok(doc) => Ok(doc),
            Err(errs) => {
                let msgs: Vec<String> = errs
                    .iter()
                    .map(|e| {
                        let loc = world.locate(e.span);
                        format!("{loc}{}{}", e.message, e.hints.iter().map(|h| format!(" (hint: {})", h.v)).collect::<String>())
                    })
                    .collect();
                bail!("template {name} failed:\n{}", msgs.join("\n"))
            }
        }
    }
}

fn file_id(path: &str) -> FileId {
    FileId::new(RootedPath::new(VirtualRoot::Project, VirtualPath::new(path).expect("valid virtual path")))
}

struct DocWorld<'a> {
    r: &'a Renderer,
    main: FileId,
    files: HashMap<FileId, Bytes>,
}

impl DocWorld<'_> {
    fn locate(&self, span: typst::syntax::DiagSpan) -> String {
        use typst::WorldExt;
        let id = match span.get() {
            typst::syntax::DiagSpanKind::Number { id, .. } | typst::syntax::DiagSpanKind::Range { id, .. } => id,
            _ => return String::new(),
        };
        let Ok(src) = self.source(id) else { return String::new() };
        let Some(range) = self.range(span) else { return String::new() };
        let line = src.text()[..range.start].matches('\n').count() + 1;
        format!("{:?}:{line}: ", id.vpath())
    }
}

impl World for DocWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.r.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.r.book
    }
    fn main(&self) -> FileId {
        self.main
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        let bytes = self.file(id)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| FileError::InvalidUtf8)?;
        Ok(Source::new(id, text.to_string()))
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.get(&id).cloned().ok_or_else(|| FileError::NotFound(id.vpath().get_with_slash().into()))
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.r.fonts.get(index).cloned()
    }
    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        let d = time::OffsetDateTime::now_utc().date();
        Datetime::from_ymd(d.year(), d.month() as u8, d.day())
    }
}

// ---------------------------------------------------------------------------
// Output encoders

pub fn to_pdf(doc: &PagedDocument) -> Result<Vec<u8>> {
    typst_pdf::pdf(doc, &typst_pdf::PdfOptions::default()).map_err(|e| anyhow!("pdf export: {:?}", e.first().map(|d| &d.message)))
}

pub fn to_png(doc: &PagedDocument, page: usize, pixel_per_pt: f64) -> Result<Vec<u8>> {
    let page = doc.pages().get(page).ok_or_else(|| anyhow!("page {page} out of range"))?;
    let opts = typst_render::RenderOptions { pixel_per_pt: typst::utils::Scalar::new(pixel_per_pt), render_bleed: false };
    typst_render::render(page, &opts).encode_png().map_err(|e| anyhow!("png: {e}"))
}

pub fn to_svg(doc: &PagedDocument, page: usize) -> Result<String> {
    let page = doc.pages().get(page).ok_or_else(|| anyhow!("page {page} out of range"))?;
    Ok(typst_svg::svg(page, &typst_svg::SvgOptions { render_bleed: false, pretty: false }))
}

// ---------------------------------------------------------------------------
// QR codes (vector, dark modules only so they sit on any background)

pub fn qr_svg(data: &str, color: &str) -> Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::M)?;
    let w = code.width();
    let colors = code.to_colors();
    let mut path = String::new();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                path.push_str(&format!("M{x} {y}h1v1h-1z"));
            }
        }
    }
    let q = 4; // quiet zone, in modules
    Ok(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" shape-rendering="crispEdges"><path fill="{color}" d="{path}"/></svg>"#,
        -(q as i64),
        -(q as i64),
        w + 2 * q,
        w + 2 * q
    ))
}

// ---------------------------------------------------------------------------
// Data model handed to templates as /data.json

pub fn link(cfg: &Config, token: &str) -> String {
    format!("{}/i/{token}", cfg.site.base_url)
}

fn site_host(cfg: &Config) -> String {
    cfg.site.base_url.trim_start_matches("https://").trim_start_matches("http://").to_string()
}

pub fn base_data(cfg: &Config, layout: Layout) -> Result<Value> {
    let d = cfg.wedding_date()?;
    let deadline = cfg.deadline()?;
    // The headline event on the invitation: "wedding" or "ceremony", else the first dated invitation event.
    let first = cfg
        .events
        .iter()
        .find(|e| (e.slug == "wedding" || e.slug == "ceremony") && e.dated())
        .or_else(|| cfg.events.iter().find(|e| e.on_invitation && e.dated()))
        .context("no dated wedding event configured")?;
    let first_t = first.start().context("headline event has no start")?;
    Ok(json!({
        "couple": {
            "a_first": cfg.couple.a_first, "a_last": cfg.couple.a_last,
            "b_first": cfg.couple.b_first, "b_last": cfg.couple.b_last,
            "monogram": cfg.monogram(), "host_line": cfg.couple.host_line,
        },
        "wedding": {
            "date_words": words::date_words(d),
            "year_words": words::year_words(d),
            "date_long": words::date_long(d),
            "date_medium": words::date_medium(d),
            "date_dots": words::date_dots(d),
            "weekday": words::weekday_name(d.weekday()),
            "day": d.day(), "month": words::month_name(d.month()), "year": d.year(),
            "location": cfg.wedding.location,
            "deadline": words::date_medium(deadline),
            "std_note": cfg.wedding.save_the_date_note,
            "return_address": cfg.wedding.return_address,
            "site": site_host(cfg),
            "first_event": {
                "name": first.name, "venue": first.venue, "address": first.address,
                "time_words": words::time_words(first_t),
            },
        },
        "theme": cfg.theme,
        "layout": layout.as_str(),
        "paper": cfg.print.paper,
        "items": [],
    }))
}

/// Build the data + QR files for a set of households.
pub fn household_data(cfg: &Config, items: &[Item], layout: Layout) -> Result<(Value, HashMap<String, Vec<u8>>)> {
    let mut data = base_data(cfg, layout)?;
    let mut files = HashMap::new();
    let mut out = vec![];
    for (i, it) in items.iter().enumerate() {
        let h = it.household;
        let url = link(cfg, &it.token);
        let qr_path = format!("/qr/{i}.svg");
        files.insert(qr_path.clone(), qr_svg(&url, &cfg.theme.ink)?.into_bytes());
        let mut events = vec![];
        for slug in &h.events {
            let Some(e) = cfg.event(slug).filter(|e| e.on_invitation) else { continue };
            let Some(t) = e.start() else { continue };
            let end = e.ends.as_deref().map(crate::config::parse_local).transpose()?;
            events.push(json!({
                "slug": e.slug, "name": e.name, "venue": e.venue, "address": e.address,
                "dress": e.dress, "note": e.note,
                "date": words::date_long(t.date()),
                "time": match end { Some(end) => format!("{} – {}", words::time_short(t), words::time_short(end)), None => words::time_short(t) },
                "time_words": words::time_words(t),
            }));
        }
        let guests: Vec<Value> = it
            .guests
            .iter()
            .map(|g| {
                let meal = it
                    .responses
                    .iter()
                    .find(|r| r.guest_id == g.id && r.attending && !r.meal.is_empty())
                    .and_then(|r| cfg.meal(&r.meal))
                    .map(|m| m.name.clone());
                let attending = it.responses.iter().any(|r| r.guest_id == g.id && r.attending);
                json!({"first": g.first_name, "last": g.last_name, "name": g.full_name(), "meal": meal, "attending": attending, "child": g.is_child})
            })
            .collect();
        out.push(json!({
            "greeting": h.greeting,
            "envelope": h.envelope_name(),
            "address": h.address_lines(),
            "url": url,
            "short_url": url.trim_start_matches("https://"),
            "qr": qr_path,
            "events": events,
            "guests": guests,
            "note": if h.australia { cfg.wedding.australia_note.clone() } else { None },
        }));
    }
    data["items"] = Value::Array(out);
    Ok((data, files))
}

/// Report data: totals per event and meal plus a row per guest.
pub fn report_data(
    cfg: &Config,
    households: &[Household],
    guests: &[Guest],
    responses: &HashMap<(i64, String), Response>,
) -> Result<Value> {
    let mut data = base_data(cfg, Layout::Card)?;
    let by_h: HashMap<i64, &Household> = households.iter().map(|h| (h.id, h)).collect();
    let mut rows = vec![];
    let mut totals: Vec<Value> = vec![];
    for e in &cfg.events {
        let invited: Vec<&Guest> = guests.iter().filter(|g| by_h.get(&g.household_id).is_some_and(|h| g.invited(h, &e.slug))).collect();
        let yes = invited.iter().filter(|g| responses.get(&(g.id, e.slug.clone())).is_some_and(|r| r.attending)).count();
        let no = invited.iter().filter(|g| responses.get(&(g.id, e.slug.clone())).is_some_and(|r| !r.attending)).count();
        let meals: Vec<Value> = cfg
            .meals
            .iter()
            .map(|m| {
                let n = invited
                    .iter()
                    .filter(|g| responses.get(&(g.id, e.slug.clone())).is_some_and(|r| r.attending && r.meal == m.slug))
                    .count();
                json!({"name": m.name, "count": n})
            })
            .filter(|v| e.meal && v["count"].as_u64() > Some(0))
            .collect();
        totals.push(json!({"name": e.name, "invited": invited.len(), "yes": yes, "no": no, "pending": invited.len() - yes - no, "meals": meals}));
    }
    for g in guests {
        let Some(h) = by_h.get(&g.household_id) else { continue };
        let cells: Vec<Value> = cfg
            .events
            .iter()
            .map(|e| {
                if !g.invited(h, &e.slug) {
                    return json!("");
                }
                match responses.get(&(g.id, e.slug.clone())) {
                    Some(r) if r.attending => json!(cfg.meal(&r.meal).map(|m| m.name.as_str()).unwrap_or("yes")),
                    Some(_) => json!("no"),
                    None => json!("?"),
                }
            })
            .collect();
        rows.push(json!({"household": h.name, "name": g.full_name(), "plus_one": g.plus_one_of.is_some(), "child": g.is_child, "cells": cells, "dietary": g.dietary}));
    }
    data["report"] = json!({
        "events": cfg.events.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
        "totals": totals,
        "rows": rows,
        "generated": crate::db::now()[..16].replace('T', " "),
    });
    Ok(data)
}

/// Convenience used by web + CLI: render households for a kind.
pub fn render_households(renderer: &Renderer, cfg: &Config, env: &Env, kind: Kind, layout: Layout, rows: &[(Household, Vec<Guest>, Vec<Response>)]) -> Result<PagedDocument> {
    let items: Vec<Item> = rows
        .iter()
        .map(|(h, g, r)| Item { household: h, guests: g, responses: r, token: crate::store::household_token(&env.secret, h) })
        .collect();
    let (data, files) = household_data(cfg, &items, layout)?;
    renderer.compile(kind, &data, files)
}
