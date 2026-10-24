#import "/lib.typ": *

// Address labels.
//   a4:        Avery L7160 / J8160: 21 per sheet, 63.5 × 38.1 mm, 3 × 7
//   us-letter: Avery 5160: 30 per sheet, 66.7 × 25.4 mm, 3 × 10
#let spec = if data.paper == "us-letter" {
  (pw: 215.9mm, ph: 279.4mm, w: 66.675mm, h: 25.4mm, cols: 3, rows: 10, top: 12.7mm, left: 4.76mm, gx: 3.175mm, gy: 0mm, name: 9.5pt, addr: 7.6pt)
} else {
  (pw: 210mm, ph: 297mm, w: 63.5mm, h: 38.1mm, cols: 3, rows: 7, top: 15.15mm, left: 7.2mm, gx: 2.54mm, gy: 0mm, name: 11pt, addr: 8.6pt)
}
#let per = spec.cols * spec.rows

#show: base-text
#set page(width: spec.pw, height: spec.ph, margin: 0pt)

#let label(it) = block(width: spec.w, height: spec.h, inset: (x: 4mm, y: 2.5mm), align(left + horizon, {
  set par(leading: 0.4em)
  text(font: display, size: spec.name, weight: 600, it.envelope)
  linebreak()
  text(font: serif, size: spec.addr, it.address.join("\n"))
}))

#let items = data.items.filter(it => it.address.len() > 0)
#for (n, page-items) in items.chunks(per).enumerate() {
  if n > 0 { pagebreak() }
  for (i, it) in page-items.enumerate() {
    let col = calc.rem(i, spec.cols)
    let row = calc.quo(i, spec.cols)
    place(dx: spec.left + col * (spec.w + spec.gx), dy: spec.top + row * (spec.h + spec.gy), label(it))
  }
}
#if items.len() == 0 [No households with a mailing address yet.]
