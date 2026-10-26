#import "/lib.typ": *
// The top of the save the date / invitation email, typeset in the site's fonts so the email
// matches the website exactly (email apps can't load web fonts). One picture, which also
// records that the email was opened.
#let W = data.wedding
#let C = data.couple
#set page(width: 104mm, height: auto, margin: (x: 1.5mm, top: 1.5mm, bottom: 2.5mm), fill: paper-fill)
#set align(center)
#set par(spacing: 0.35em)

#monogram(size: 44mm)
#v(5mm)
#caps(data.header_kicker, size: 10.5pt, tracking: 0.4em)
#v(3.5mm)
#let w = data.prose.website.at("the_wedding_of", default: "")
#if w != "" {
  text(font: display, style: "italic", size: 13.5pt, fill: muted, w)
  v(1mm)
}
#text(font: display, size: 31pt, weight: 300)[#C.a_first#h(0.18em)#text(style: "italic", size: 20pt, fill: accent)[&]#h(0.18em)#C.b_first]
#v(4.5mm)
#diamond-rule(width: 40mm)
#v(4.5mm)
#text(font: serif, size: 10.5pt, tracking: 0.22em, upper(W.weekday) + h(0.4em) + "·" + h(0.4em) + str(W.day) + " " + upper(W.month) + h(0.4em) + "·" + h(0.4em) + str(W.year))
#v(2mm)
#text(font: display, style: "italic", size: 13.5pt, fill: muted, W.location)
