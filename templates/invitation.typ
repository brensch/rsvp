#import "/lib.typ": *

#let W = data.wedding
#let C = data.couple
#let E = W.first_event

#let front(it) = {
  frame(card-w, card-h)
  block(width: 100%, height: 100%, inset: (x: 14mm, top: 18mm, bottom: 16mm), align(center, {
    v(1fr)
    monogram(size: 22mm)
    v(7mm)
    text(font: display, style: "italic", size: 13pt, fill: muted, C.host_line)
    v(7mm)
    text(font: display, size: 21pt, tracking: 0.14em, weight: 400, upper(C.a_first + " " + C.a_last))
    v(2mm)
    amp(size: 24pt)
    v(1.5mm)
    text(font: display, size: 21pt, tracking: 0.14em, weight: 400, upper(C.b_first + " " + C.b_last))
    v(7mm)
    block(width: 88mm, {
      set par(leading: 0.6em)
      text(font: display, style: "italic", size: 13pt)[request the pleasure of your company\ at the celebration of their marriage]
    })
    v(7mm)
    diamond-rule(width: 36mm)
    v(6mm)
    {
      set par(leading: 0.62em)
      text(font: display, size: 14pt)[#W.date_words\ #W.year_words\ at #E.time_words]
    }
    v(6mm)
    caps(E.venue, size: 9pt, fill: ink, tracking: 0.24em)
    v(2mm)
    text(font: display, style: "italic", size: 12.5pt, fill: muted, if E.address != "" { E.address } else { W.location })
    v(1.2fr)
  }))
}

#let back(it) = {
  frame(card-w, card-h)
  block(width: 100%, height: 100%, inset: (x: 15mm, top: 17mm, bottom: 15mm), align(center, {
    caps("The details", size: 7pt, tracking: 0.42em)
    v(3.5mm)
    text(font: script, size: 21pt, fill: accent)[for #it.greeting]
    v(1fr)
    for (i, e) in it.events.enumerate() {
      if i > 0 { v(5mm); thin-rule(width: 12mm); v(5mm) }
      caps(e.name, size: 8pt, fill: ink, tracking: 0.32em)
      v(2.6mm)
      text(font: display, size: 12.5pt)[#e.date]
      v(1.4mm)
      text(font: display, size: 12.5pt)[#e.time]
      v(1.8mm)
      text(font: display, size: 13pt, style: "italic", fill: accent)[#e.venue]
      if e.address != "" { v(1.2mm); text(size: 8.5pt, fill: muted, e.address) }
      if e.dress != none { v(1.8mm); caps("Dress · " + e.dress, size: 6.5pt, fill: muted, tracking: 0.2em) }
    }
    if it.note != none {
      v(6mm)
      block(width: 86mm, { set par(leading: 0.55em); text(font: display, style: "italic", size: 10.5pt, fill: accent, it.note) })
    }
    v(1fr)
    diamond-rule(width: 30mm)
    v(5mm)
    text(font: display, size: 13pt)[Kindly respond by #W.deadline]
    v(4mm)
    qr(it, size: 24mm)
    v(2.5mm)
    text(size: 7.5pt, fill: muted, it.short_url)
  }))
}

#cards(data.items, front, back)
