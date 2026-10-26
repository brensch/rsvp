// Shared design system for all stationery.
// Classic editorial: Cormorant Garamond display, EB Garamond text, Pinyon Script accents,
// fine double rules, a procedurally drawn laurel wreath and diamond rules.

#let data = json("/data.json")
#let T = data.theme
#let ink = rgb(T.ink)
#let muted = rgb(T.muted)
#let accent = rgb(T.accent)
#let paper-fill = rgb(T.paper)
#let rule-c = rgb(T.rule)

#let display = "Cormorant Garamond"
#let serif = "EB Garamond"
#let script = "Pinyon Script"

#let card-w = 127mm // 5 x 7 in
#let card-h = 178mm
#let bleed = 3mm

// Tracked uppercase label.
#let caps(body, size: 8pt, tracking: 0.3em, fill: accent, weight: 500) = text(
  font: serif, size: size, tracking: tracking, fill: fill, weight: weight, upper(body),
)

#let diamond(size: 2mm, fill: accent) = box(
  width: size * 1.42, height: size * 1.42,
  align(center + horizon, rotate(45deg, rect(width: size, height: size, fill: fill, stroke: none))),
)

// --- ◆ ---
#let diamond-rule(width: 44mm, color: rule-c) = box(width: width, grid(
  columns: (1fr, auto, 1fr), column-gutter: 2.2mm, align: horizon,
  line(length: 100%, stroke: 0.45pt + color),
  diamond(size: 1.5mm),
  line(length: 100%, stroke: 0.45pt + color),
))

#let thin-rule(width: 20mm, color: rule-c) = box(width: width, line(length: 100%, stroke: 0.45pt + color))

// The emblem: golden wattle (Australia) and olive (USA) rising from a ribbon bow into an open
// wreath. Drawn by tools/draw_emblem.py into /art/emblem.svg.
#let art-ratio = 520 / 600 // height / width

#let emblem(width: 60mm) = image("/art/emblem.svg", width: width)

// The emblem with the couple's initials ("A & B") in the middle. `size` is its width.
#let monogram(size: 60mm) = box(width: size, height: size * art-ratio, {
  place(emblem(width: size))
  let m = data.couple.monogram.clusters()
  place(center + top, dy: size * art-ratio * 0.31, {
    set text(font: display, style: "italic", fill: ink, size: size * 0.22, weight: 400)
    if m.len() == 2 {
      [#m.at(0)#h(0.04em)#text(size: size * 0.135, fill: accent)[&]#h(0.04em)#m.at(1)]
    } else {
      data.couple.monogram
    }
  })
})

// Double-rule border inset from the trim edge.
#let frame(w, h, inset: 6.5mm, gap: 1.3mm) = {
  place(dx: inset, dy: inset, rect(width: w - 2 * inset, height: h - 2 * inset, stroke: 0.75pt + rule-c))
  place(dx: inset + gap, dy: inset + gap, rect(width: w - 2 * (inset + gap), height: h - 2 * (inset + gap), stroke: 0.3pt + rule-c))
  for (x, y) in ((inset, inset), (w - inset, inset), (inset, h - inset), (w - inset, h - inset)) {
    place(dx: x - 1.07mm, dy: y - 1.07mm, diamond(size: 1.5mm, fill: rule-c))
  }
}

// Crop marks just outside a trim box at (x, y, w, h).
#let crop-marks(x, y, w, h, off: 2mm, len: 3mm) = {
  let s = 0.25pt + black
  for (cx, cy, dx, dy) in ((x, y, -1, -1), (x + w, y, 1, -1), (x, y + h, -1, 1), (x + w, y + h, 1, 1)) {
    place(dx: cx + dx * off - (if dx < 0 { len } else { 0pt }), dy: cy, line(length: len, stroke: s))
    place(dx: cx, dy: cy + dy * off - (if dy < 0 { len } else { 0pt }), line(angle: 90deg, length: len, stroke: s))
  }
}

// Display ampersand: Cormorant italic, which is more legible than the script one at size.
#let amp(size: 24pt) = text(font: display, style: "italic", size: size, fill: accent, weight: 400)[&]

#let qr(it, size: 28mm) = image(it.qr, width: size, height: size)

#let base-text(body) = {
  set text(font: serif, fill: ink, size: 11pt, hyphenate: false, number-type: "old-style")
  set par(spacing: 0pt, leading: 0.55em, justify: false)
  set block(spacing: 0pt)
  body
}

// Lay out two-sided cards.
//   card:  one card per page at trim size with bleed (for a print shop)
//   sheet: two per landscape A4/Letter page with crop marks; back sheets are
//          mirrored left-right so they line up when duplexed.
#let cards(items, front, back, w: card-w, h: card-h) = {
  show: base-text
  if data.layout == "card" {
    set page(width: w, height: h, bleed: bleed, margin: 0pt, fill: paper-fill)
    for (i, it) in items.enumerate() {
      if i > 0 { pagebreak() }
      block(width: w, height: h, front(it))
      pagebreak()
      block(width: w, height: h, back(it))
    }
  } else {
    let (pw, ph) = if data.paper == "us-letter" { (279.4mm, 215.9mm) } else { (297mm, 210mm) }
    set page(width: pw, height: ph, margin: 0pt)
    let gx = calc.min(12mm, (pw - 2 * w) / 3)
    let mx = (pw - 2 * w - gx) / 2
    let my = (ph - h) / 2
    let xs = (mx, mx + w + gx)
    let note(t) = place(dx: mx, dy: ph - 6mm, text(size: 6pt, fill: luma(150), font: serif, t))
    for (n, chunk) in items.chunks(2).enumerate() {
      if n > 0 { pagebreak() }
      for (j, it) in chunk.enumerate() {
        place(dx: xs.at(j), dy: my, block(width: w, height: h, clip: true, front(it)))
        crop-marks(xs.at(j), my, w, h)
      }
      note[Fronts · sheet #(n + 1). Print at 100% (no scaling) on card stock, duplex flipping left–right.]
      pagebreak()
      for (j, it) in chunk.enumerate() {
        let x = xs.at(1 - j) // mirrored for duplex
        place(dx: x, dy: my, block(width: w, height: h, clip: true, back(it)))
        crop-marks(x, my, w, h)
      }
      note[Backs · sheet #(n + 1)]
    }
  }
}

#let sheet-size() = if data.paper == "us-letter" { (215.9mm, 279.4mm) } else { (210mm, 297mm) }
