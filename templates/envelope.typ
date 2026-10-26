#import "/lib.typ": *

// A7 envelope (7.25 × 5.25 in) for 5 × 7 cards. Feed envelopes through the printer
// as a custom page size. Change W/H here for C6 (162 × 114 mm) or others.
#let W = 184.2mm
#let H = 133.4mm

#show: base-text
#set page(width: W, height: H, margin: 0pt)

#for (i, it) in data.items.enumerate() {
  if i > 0 { pagebreak() }
  // return address
  place(dx: 12mm, dy: 11mm, {
    set par(leading: 0.45em)
    text(font: display, style: "italic", size: 9.5pt, fill: muted, data.wedding.return_address.join("\n"))
  })
  place(dx: W - 36mm, dy: 9mm, monogram(size: 26mm))
  // recipient
  place(center + horizon, dy: 8mm, block(width: 120mm, align(center, {
    text(font: script, size: 25pt, fill: ink, it.envelope)
    v(5mm)
    set par(leading: 0.62em)
    for line in it.address {
      text(font: display, size: 13pt, tracking: 0.06em, line)
      linebreak()
    }
  })))
}
