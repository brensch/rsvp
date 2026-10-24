#import "/lib.typ": *

// Flat place cards, 90 × 55 mm, laid out on A4/Letter with crop marks.
// Only guests who are attending at least one event. The small line under the
// name is the meal, for the catering staff.
#let cw = 90mm
#let ch = 55mm
#let (pw, ph) = sheet-size()
#let cols = 2
#let rows = calc.floor((ph - 20mm) / (ch + 4mm))
#let gx = 6mm
#let gy = 4mm
#let mx = (pw - cols * cw - (cols - 1) * gx) / 2
#let my = (ph - rows * ch - (rows - 1) * gy) / 2

#show: base-text
#set page(width: pw, height: ph, margin: 0pt)

#let people = data.items.map(it => it.guests.filter(g => g.attending)).flatten()

#let card(g) = block(width: cw, height: ch, {
  place(dx: 3mm, dy: 3mm, rect(width: cw - 6mm, height: ch - 6mm, stroke: 0.4pt + rule-c))
  align(center + horizon, {
    text(font: script, size: if g.name.len() > 18 { 20pt } else { 25pt }, fill: ink, g.name)
    v(3mm)
    diamond-rule(width: 22mm)
    if g.meal != none {
      v(2.5mm)
      caps(g.meal, size: 6.5pt, fill: muted)
    }
  })
})

#for (n, chunk) in people.chunks(cols * rows).enumerate() {
  if n > 0 { pagebreak() }
  for (i, g) in chunk.enumerate() {
    let x = mx + calc.rem(i, cols) * (cw + gx)
    let y = my + calc.quo(i, cols) * (ch + gy)
    place(dx: x, dy: y, card(g))
    crop-marks(x, y, cw, ch, off: 1mm, len: 2mm)
  }
}
#if people.len() == 0 [No attending guests yet. Place cards appear once people RSVP.]
