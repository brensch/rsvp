"""Bow preview page: the emblem (templates/art/emblem.svg) without its bow, with candidate bows drawn on.

    python3 tools/bow_preview.py templates/art/emblem.svg bows.html

Served as a static page at weddedblyss.com/bows by Caddy (outside the app), using the site's fonts.
"""
import re, sys

src, out = sys.argv[1], sys.argv[2]
e = open(src).read()
inner = e[e.find(">", e.find("<svg")) + 1 : e.rfind("</svg>")]
# Drop the drawn bow (the ribbon colours from draw_emblem.py's bow()), keep everything else.
RIBBON_COLOURS = ("#a0702f", "#6f4b1f", "#d4a764")
lines = [l for l in inner.split("\n") if not (l.startswith(("<path", "<ellipse")) and any(c in l for c in RIBBON_COLOURS))]
symbol = '<symbol id="laurel" viewBox="0 0 600 520">' + "\n".join(lines) + "</symbol>"

def mirror(d):
    return re.sub(r'(-?\d+(?:\.\d+)?),(-?\d+(?:\.\d+)?)', lambda m: f"{600-float(m.group(1)):g},{m.group(2)}", d)

LOOP = "M300,434 C284,408 252,412 258,430 C262,444 286,440 300,434"
TAIL = "M300,434 C290,456 270,462 262,480 C254,498 266,512 276,504 C282,498 276,490 270,494"

CLARET = ("#8c1c2b", "#480e16", "#cd4b5c")

def bow(w=5.6, loop=LOOP, tail=TAIL, finish="flat"):
    paths = [tail, mirror(tail), loop, mirror(loop)]
    cap = 'stroke-linecap="round" stroke-linejoin="round" fill="none"'
    s = []
    for d in paths:
        if finish == "flat":
            s.append(f'<path d="{d}" stroke="var(--b)" stroke-width="{w}" {cap}/>')
        elif finish == "edged":
            s.append(f'<path d="{d}" stroke="var(--s)" stroke-width="{w+1.8:g}" {cap}/>')
            s.append(f'<path d="{d}" stroke="var(--b)" stroke-width="{w-0.6:g}" {cap}/>')
        elif finish == "sheen":
            s.append(f'<path d="{d}" stroke="var(--s)" stroke-width="{w+1.2:g}" {cap}/>')
            s.append(f'<path d="{d}" stroke="var(--b)" stroke-width="{w-0.4:g}" {cap}/>')
            s.append(f'<path d="{d}" stroke="var(--h)" stroke-width="{w*0.22:.2f}" opacity=".55" transform="translate(-0.7,-0.7)" {cap}/>')
        elif finish == "twist":
            s.append(f'<path d="{d}" stroke="var(--s)" stroke-width="{w+1.2:g}" {cap}/>')
            s.append(f'<path d="{d}" stroke="var(--b)" stroke-width="{w-0.4:g}" {cap}/>')
            s.append(f'<path d="{d}" stroke="var(--s)" stroke-width="{w-0.4:g}" stroke-dasharray="1.1 2.4" opacity=".55" fill="none"/>')
    r = w * 1.6
    if finish == "flat":
        s.append(f'<circle cx="300" cy="434" r="{r:.1f}" fill="var(--s)"/>')
    else:
        s.append(f'<circle cx="300" cy="434" r="{r+0.9:.1f}" fill="var(--s)"/>')
        s.append(f'<circle cx="300" cy="434" r="{r-0.6:.1f}" fill="var(--b)"/>')
        s.append(f'<path d="M{300-r*0.55:.1f},{434-r*0.25:.1f} Q300,{434-r*0.75:.1f} {300+r*0.55:.1f},{434-r*0.25:.1f}" stroke="var(--h)" stroke-width="1" opacity=".6" fill="none" stroke-linecap="round"/>')
    return "".join(s)

def emblem(bowsvg, label, cls="full"):
    return (f'<svg class="{cls}" viewBox="0 0 600 540" role="img" aria-label="{label}"><use href="#laurel"/>{bowsvg}</svg>')

def zoom(bowsvg):
    return f'<svg class="zoom" viewBox="212 388 176 148" aria-hidden="true"><use href="#laurel"/>{bowsvg}</svg>'

def style(c):
    return f"--b:{c[0]};--s:{c[1]};--h:{c[2]}"

def card(code, name, note, bowsvg, colours=CLARET, pick=False):
    tag = '<span class="now">Your pick</span>' if pick else ""
    return (f'<figure class="card{" is-pick" if pick else ""}" style="{style(colours)}">'
            f'<div class="proof">{emblem(bowsvg, name)}{zoom(bowsvg)}</div>'
            f'<figcaption><span class="code">{code}</span><b>{name}</b>{tag}<p>{note}</p></figcaption></figure>')

sections = []

sections.append(("Weight", "How thick the cord is. Everything else stays as your pick.", [
    card("W1", "Fine cord", "4.2 wide. Closer to twine, lets the poppies lead.", bow(w=4.2)),
    card("W2", "Thick", "5.6 wide, as picked.", bow(), pick=True),
    card("W3", "Heavy", "7.0 wide. Reads clearly at small sizes like the email header.", bow(w=7.0)),
    card("W4", "Chunky", "8.4 wide. Bold, almost a velvet rope.", bow(w=8.4)),
]))

sections.append(("Curl", "The shape of the tails and loops.", [
    card("C1", "As picked", "Tails sweep out, then curl back towards the centre.", bow(), pick=True),
    card("C2", "Tighter curl", "Smaller, neater hook at each tail end.",
         bow(tail="M300,434 C290,456 270,462 264,480 C259,496 268,506 275,501 C280,497 276,491 271,493")),
    card("C3", "Full spiral", "The curl keeps turning inward for one more half turn.",
         bow(tail=TAIL + " C266,497 268,502 272,500")),
    card("C4", "Outward curl", "Tails curl away from the centre, echoing the outer stems.",
         bow(tail="M300,434 C290,456 274,466 268,484 C262,502 248,510 242,500 C238,492 246,488 251,494")),
    card("C5", "Long and flowing", "Longer tails that hang lower before they curl.",
         bow(tail="M300,434 C292,462 266,468 258,490 C250,512 264,528 276,520 C284,514 278,504 271,508")),
    card("C6", "Bigger loops", "Wider bow loops, same tails.",
         bow(loop="M300,434 C280,400 240,402 246,428 C250,448 284,444 300,434")),
    card("C7", "Small round loops", "Compact loops so the tails do the talking.",
         bow(loop="M300,434 C288,414 262,416 266,432 C269,443 288,440 300,434")),
    card("C8", "Big loops, long tails", "C5 and C6 together. The most generous version.",
         bow(loop="M300,434 C280,400 240,402 246,428 C250,448 284,444 300,434",
             tail="M300,434 C292,462 266,468 258,490 C250,512 264,528 276,520 C284,514 278,504 271,508")),
]))

sections.append(("Finish", "How the cord is shaded. Most visible in the close-ups.", [
    card("F1", "Flat", "One solid colour, as picked. Cleanest when printed small.", bow(), pick=True),
    card("F2", "Edged", "A darker edge so it reads as a round cord.", bow(finish="edged")),
    card("F3", "Sheen", "Edged, plus a soft highlight along the top like silk cord.", bow(finish="sheen")),
    card("F4", "Twisted", "Edged with a fine twist texture. Closer to rope.", bow(finish="twist")),
]))

SHADES = [
    ("S1", "Claret", "#8c1c2b", "Deep wine red, as picked.", CLARET),
    ("S2", "Bright claret", "#a3243a", "A touch lighter and redder; sits closer to the poppies.", ("#a3243a", "#5a1220", "#d9606f")),
    ("S3", "Deep claret", "#6e1522", "Darker and more formal; almost burgundy.", ("#6e1522", "#3a0a12", "#b0404f")),
    ("S4", "Oxblood", "#7c2724", "Browner red. Ties back to the brown stems.", ("#7c2724", "#421412", "#b85a52")),
    ("S5", "Berry", "#8a1f45", "Cooler, pinker red. Contrasts more with the orange.", ("#8a1f45", "#470f23", "#c9557e")),
]
sections.append(("Shade", "Small moves around claret.", [
    card(code, name, f'<code>{hexv}</code> {note}', bow(), colours=cols, pick=(code == "S1"))
    for code, name, hexv, note, cols in SHADES
]))

sizes = "".join(
    f'<figure class="size"><div class="sizebox" style="{style(CLARET)}">'
    f'<svg viewBox="0 0 600 540" style="width:{px}px" role="img" aria-label="Emblem at {px} pixels"><use href="#laurel"/>{bow()}</svg></div>'
    f'<figcaption>{px}px · {where}</figcaption></figure>'
    for px, where in [(40, "browser tab"), (96, "email header"), (180, "site header"), (320, "invitation")]
)

hero_bow = bow()
body = []
for title, sub, cards in sections:
    sid = title.lower()
    body.append(f'<section id="{sid}"><header class="sec"><h2>{title}</h2><p>{sub}</p></header><div class="grid">{"".join(cards)}</div></section>')

html = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Curled cord in claret</title>
<meta name="robots" content="noindex,nofollow">
<link rel="icon" type="image/png" sizes="32x32" href="/favicon.ico">
<style>
@font-face{{font-family:"Cormorant Garamond";font-weight:300 500;src:url(/fonts/CormorantGaramond-v2.woff2) format("woff2")}}
@font-face{{font-family:"EB Garamond";font-weight:400 500;src:url(/fonts/EBGaramond-v2.woff2) format("woff2")}}
/* Layout: a proof sheet. The emblem always sits on invitation paper; the table around it follows the viewer's theme. */
:root {{
  --paper:#fbf8f1; --card:#fffdf8; --ink:#24211d; --muted:#6a6052; --rule:#d8cbb0; --claret:#8c1c2b;
  --table:#f1ebdf; --text:#24211d; --text-muted:#5f574b; --line:#cfc1a4;
  --display:"Cormorant Garamond","Cormorant",Georgia,"Times New Roman",serif;
  --body:"EB Garamond",Georgia,"Times New Roman",serif;
}}
@media (prefers-color-scheme: dark) {{ :root:not([data-theme="light"]) {{ --table:#1d1917; --text:#efe7da; --text-muted:#b8ab98; --line:#4a4038; --claret:#d0606f; color-scheme:dark }} }}
:root[data-theme="dark"] {{ --table:#1d1917; --text:#efe7da; --text-muted:#b8ab98; --line:#4a4038; --claret:#d0606f; color-scheme:dark }}
* {{ box-sizing:border-box }}
body {{ background:var(--table); color:var(--text); font:400 1.05rem/1.55 var(--body);
  padding-block:40px 72px; padding-left:max(16px, env(safe-area-inset-left, 0px)); padding-right:max(16px, env(safe-area-inset-right, 0px)) }}
.wrap {{ max-width:1240px; margin:0 auto; display:grid; gap:56px }}
h1, h2 {{ font-family:var(--display); font-weight:300; text-wrap:balance; margin:0 }}
h1 {{ font-size:clamp(2.4rem, 6vw, 3.6rem); line-height:1.02 }}
h2 {{ font-size:2.1rem; line-height:1.1 }}
.eyebrow {{ font-size:.78rem; letter-spacing:.16em; text-transform:uppercase; color:var(--text-muted); margin:0 0 10px }}
.lede {{ max-width:60ch; color:var(--text-muted); margin:14px 0 0 }}
.hero {{ display:grid; grid-template-columns:minmax(0,1.25fr) minmax(0,1fr); gap:32px; align-items:center }}
.hero .proofbig {{ background:var(--paper); border:1px solid var(--line); padding:20px; display:grid; grid-template-columns:minmax(0,1.5fr) minmax(0,1fr); gap:12px; align-items:center }}
.hero .proofbig svg {{ width:100%; height:auto; display:block }}
.hero .proofbig .zoombig {{ border-left:1px solid #eee5d3; padding-left:12px }}
.spec {{ display:grid; grid-template-columns:auto 1fr; gap:6px 18px; margin:22px 0 0; font-size:1rem }}
.spec dt {{ color:var(--text-muted); font-size:.8rem; letter-spacing:.12em; text-transform:uppercase; padding-top:3px }}
.spec dd {{ margin:0 }}
.sw {{ display:inline-block; width:.8em; height:.8em; border-radius:50%; background:#8c1c2b; vertical-align:-1px; margin-right:6px; border:1px solid rgba(0,0,0,.2) }}
.how {{ margin:22px 0 0; padding:14px 16px; border:1px dashed var(--line); font-style:italic; color:var(--text-muted) }}
.how b {{ font-style:normal; color:var(--text); font-weight:500 }}
section {{ display:grid; gap:20px }}
.sec p {{ margin:4px 0 0; color:var(--text-muted); font-style:italic }}
.grid {{ display:grid; grid-template-columns:repeat(auto-fill, minmax(min(100%, 270px), 1fr)); gap:18px }}
.card {{ margin:0; background:var(--card); color:var(--ink); border:1px solid var(--rule); padding:14px 14px 16px; display:grid; gap:10px; align-content:start }}
.card.is-pick {{ outline:2px solid var(--claret); outline-offset:-1px }}
.proof {{ background:var(--paper); display:grid; gap:4px }}
.proof .full {{ width:100%; height:auto; display:block }}
.proof .zoom {{ width:62%; height:auto; display:block; margin:0 auto; border-top:1px solid #eee5d3; padding-top:4px }}
figcaption b {{ font-weight:500; font-size:1.08rem }}
figcaption p {{ margin:4px 0 0; color:var(--muted); font-size:.95rem; line-height:1.45 }}
.code {{ display:inline-block; min-width:2.4em; font:500 .78rem/1 var(--body); letter-spacing:.08em; padding:4px 6px; margin-right:8px; border:1px solid currentColor; color:#8c1c2b; text-align:center; vertical-align:2px; font-variant-numeric:tabular-nums }}
.now {{ font-size:.72rem; letter-spacing:.12em; text-transform:uppercase; color:#8c1c2b; margin-left:8px }}
code {{ font-size:.82rem; color:var(--muted) }}
.sizes {{ display:flex; flex-wrap:wrap; gap:18px; align-items:flex-end }}
.size {{ margin:0; display:grid; gap:8px; justify-items:center; min-width:0 }}
.sizebox {{ background:var(--paper); border:1px solid var(--line); padding:12px; display:grid; place-items:center; max-width:100% }}
.sizebox svg {{ height:auto; display:block; max-width:100% }}
.size figcaption {{ color:var(--text-muted); font-size:.92rem }}
@media (max-width:760px) {{ .hero {{ grid-template-columns:minmax(0,1fr) }} }}
</style>
</head><body>
<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs>{symbol}</defs></svg>
<main class="wrap">
  <div class="hero">
    <div class="proofbig" style="{style(CLARET)}">
      <svg viewBox="0 0 600 540" role="img" aria-label="Emblem with thick curled claret cord">{f'<use href="#laurel"/>{hero_bow}'}</svg>
      <svg class="zoombig" viewBox="212 388 176 148" aria-hidden="true"><use href="#laurel"/>{hero_bow}</svg>
    </div>
    <div>
      <p class="eyebrow">Wedding emblem · bow round 2</p>
      <h1>Curled cord in claret</h1>
      <p class="lede">Your pick from the bow styles page, with variations one at a time: weight, curl, finish and shade. Then the pick at the sizes it will actually be used.</p>
      <dl class="spec">
        <dt>Shape</dt><dd>Curled tails, thick cord</dd>
        <dt>Colour</dt><dd><span class="sw"></span>Claret <code>#8c1c2b</code></dd>
        <dt>Cord</dt><dd>5.6 wide, flat colour, dark knot</dd>
      </dl>
      <p class="how">Reply with one code from each row you want changed, for example <b>W3 · C5 · F2 · S1</b>. Outlined cards are the current pick.</p>
    </div>
  </div>
  {"".join(body)}
  <section id="sizes"><header class="sec"><h2>At real sizes</h2><p>The current pick, shown at the sizes it will be used.</p></header><div class="sizes">{sizes}</div></section>
</main>
</body></html>
"""
open(out, "w").write(html)
print(len(html))
