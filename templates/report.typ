#import "/lib.typ": *

#let R = data.report
#let (pw, ph) = sheet-size()

#show: base-text
#set page(width: pw, height: ph, margin: (x: 14mm, top: 16mm, bottom: 14mm), footer: context align(center, text(size: 7pt, fill: muted)[
  #data.couple.a_first & #data.couple.b_first · RSVP report · #R.generated UTC · page #counter(page).display()
]))
#set text(size: 8.5pt, number-type: "lining")

#align(center)[
  #text(font: display, size: 22pt)[#data.couple.a_first & #data.couple.b_first]
  #v(1.5mm)
  #caps(data.wedding.date_long + " · " + data.wedding.location, size: 7pt)
]
#v(7mm)

#grid(columns: (1fr,) * calc.max(1, R.totals.len()), column-gutter: 5mm, ..R.totals.map(t => block(
  stroke: 0.5pt + rule-c, inset: 3.5mm, width: 100%, {
    caps(t.name, size: 7pt, fill: accent)
    v(2mm)
    text(font: display, size: 20pt)[#t.yes]
    text(size: 8pt, fill: muted)[ attending]
    v(1mm)
    text(size: 8pt, fill: muted)[#t.no declined · #t.pending awaiting · #t.invited invited]
    if t.meals.len() > 0 {
      v(2mm)
      for m in t.meals [#m.name: *#m.count* \ ]
    }
  },
)))

#v(7mm)
#let cols = (auto, auto) + R.events.map(_ => auto) + (1fr,)
#table(
  columns: cols,
  stroke: (x, y) => if y == 0 { (bottom: 0.6pt + ink) } else { (bottom: 0.3pt + rule-c) },
  inset: (x: 2.2mm, y: 1.7mm),
  table.header(
    caps("Household", size: 6.5pt, fill: ink), caps("Guest", size: 6.5pt, fill: ink),
    ..R.events.map(e => caps(e, size: 6.5pt, fill: ink)),
    caps("Dietary", size: 6.5pt, fill: ink),
  ),
  ..R.rows.map(r => (
    text(fill: muted, r.household),
    [#r.name#if r.plus_one [ #text(fill: muted)[(+1)]]#if r.child [ #text(fill: muted)[(child)]]],
    ..r.cells.map(c => if c == "no" { text(fill: muted)[no] } else if c == "?" { text(fill: accent)[?] } else [#c]),
    r.dietary,
  )).flatten()
)
