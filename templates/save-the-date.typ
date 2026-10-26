#import "/lib.typ": *

#let W = data.wedding
#let C = data.couple

#let front(it) = {
  frame(card-w, card-h)
  block(width: 100%, height: 100%, inset: (x: 15mm, top: 19mm, bottom: 17mm), align(center, {
    caps("Save the date", size: 12pt, tracking: 0.4em)
    v(1fr)
    monogram(size: 58mm)
    v(6mm)
    text(font: display, size: 33pt, weight: 300, tracking: 0.01em)[#C.a_first]
    v(4mm)
    text(font: display, style: "italic", size: 17pt, fill: accent)[and]
    v(2.5mm)
    text(font: display, size: 33pt, weight: 300, tracking: 0.01em)[#C.b_first]
    v(7mm)
    diamond-rule(width: 50mm)
    v(6mm)
    text(font: serif, size: 10pt, tracking: 0.26em, upper(W.weekday) + h(0.5em) + "·" + h(0.5em) + str(W.day) + " " + upper(W.month) + h(0.5em) + "·" + h(0.5em) + str(W.year))
    v(3mm)
    text(font: display, style: "italic", size: 14pt, fill: muted, W.location)
    v(1.3fr)
    text(font: display, style: "italic", size: 11pt, fill: muted, W.std_note)
  }))
}

#let back(it) = {
  frame(card-w, card-h)
  block(width: 100%, height: 100%, inset: (x: 16mm, top: 22mm, bottom: 17mm), align(center, {
    text(font: display, style: "italic", size: 21pt, fill: accent)[Dear #it.greeting,]
    v(6mm)
    block(width: 92mm, {
      set par(leading: 0.62em)
      text(font: display, size: 14.5pt)[We're getting married,\ and we would love you to be there.]
      if data.wedding.at("celebration_note", default: none) != none {
        v(4mm)
        text(font: display, style: "italic", size: 11.5pt, fill: muted, data.wedding.celebration_note)
      }
    })
    v(1fr)
    diamond-rule(width: 30mm)
    v(7mm)
    caps("When", size: 6.5pt)
    v(2mm)
    text(font: display, size: 14pt)[#W.date_long]
    v(5mm)
    caps("Where", size: 6.5pt)
    v(2mm)
    text(font: display, size: 14pt)[#W.location]
    v(1.4fr)
    qr(it, size: 27mm)
    v(3mm)
    caps("Scan for details & to RSVP", size: 6.2pt, fill: muted, tracking: 0.22em)
    v(1.6mm)
    text(size: 7.5pt, fill: muted, font: serif, it.short_url)
  }))
}

#cards(data.items, front, back)
