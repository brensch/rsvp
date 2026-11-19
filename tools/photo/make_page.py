"""Build index.html for the /photo comparison page from <dir>/variants.json, then remove the json.

usage: make_page.py <dir>
"""
import html, json, os, sys

d = sys.argv[1]
m = json.load(open(os.path.join(d, "variants.json")))
cards = []
for v in m:
    n, f = v["n"], v["file"]
    tag = ' <span class="tag">on the site now</span>' if n == 1 else (' <span class="tag rec">recommended</span>' if "recommended" in v["title"] else "")
    cards.append(f'''<figure class="card" id="v{n}">
  <a href="/photo/{f}-full.jpg" class="shot"><img src="/photo/{f}.jpg" width="1000" height="1047" loading="{"eager" if n < 4 else "lazy"}" alt="Variant {n}: {html.escape(v['title'])}"><span class="num">{n}</span></a>
  <figcaption><b>{html.escape(v['title'])}</b>{tag}<p>{html.escape(v['note'])}</p></figcaption>
</figure>''')

page = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Photo edits</title>
<meta name="robots" content="noindex,nofollow">
<link rel="icon" type="image/png" sizes="32x32" href="/favicon.ico">
<style>
@font-face{{font-family:"Cormorant Garamond";font-weight:300 500;src:url(/fonts/CormorantGaramond-v2.woff2) format("woff2")}}
@font-face{{font-family:"EB Garamond";font-weight:400 500;src:url(/fonts/EBGaramond-v2.woff2) format("woff2")}}
/* A contact sheet: numbered prints on the site's paper, so the couple can reply with a number. */
:root{{--paper:#fbf8f1;--card:#fffdf8;--ink:#24211d;--muted:#6a6052;--rule:#d8cbb0;--claret:#8c1c2b;--table:#f1ebdf;--text:#24211d;--text-muted:#5f574b;--line:#cfc1a4;
 --display:"Cormorant Garamond",Georgia,serif;--body:"EB Garamond",Georgia,serif}}
@media (prefers-color-scheme:dark){{:root{{--table:#1d1917;--text:#efe7da;--text-muted:#b8ab98;--line:#4a4038;color-scheme:dark}}}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--table);color:var(--text);font:400 1.05rem/1.55 var(--body);padding-block:40px 72px;padding-left:max(16px,env(safe-area-inset-left,0px));padding-right:max(16px,env(safe-area-inset-right,0px))}}
.wrap{{max-width:1240px;margin:0 auto;display:grid;gap:28px}}
h1{{font:300 clamp(2.4rem,6vw,3.4rem)/1.05 var(--display);margin:0;text-wrap:balance}}
.eyebrow{{font-size:.78rem;letter-spacing:.16em;text-transform:uppercase;color:var(--text-muted);margin:0 0 10px}}
.lede{{max-width:62ch;color:var(--text-muted);margin:12px 0 0}}
.grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(min(100%,340px),1fr));gap:20px}}
.card{{margin:0;background:var(--card);color:var(--ink);border:1px solid var(--rule);padding:12px 12px 14px;display:grid;gap:10px;align-content:start}}
.shot{{position:relative;display:block}}
.shot img{{display:block;width:100%;height:auto;background:var(--paper)}}
.num{{position:absolute;top:10px;left:10px;min-width:2.6rem;padding:4px 10px;background:rgba(36,33,29,.82);color:#fff;font:500 1.5rem/1.2 var(--display);text-align:center}}
figcaption b{{font-weight:500;font-size:1.1rem}}
figcaption p{{margin:4px 0 0;color:var(--muted);font-size:.95rem;line-height:1.45}}
.tag{{font-size:.7rem;letter-spacing:.12em;text-transform:uppercase;color:var(--claret);margin-left:8px}}
.how{{padding:12px 16px;border:1px dashed var(--line);font-style:italic;color:var(--text-muted)}}
</style></head><body>
<main class="wrap">
  <header>
    <p class="eyebrow">Save the date photo · edits</p>
    <h1>Pick a photo edit</h1>
    <p class="lede">The same photo with different edits. 0 is the photo before any colour work and 1 is what's on the site now. Tap a photo to see it full size.</p>
  </header>
  <p class="how">Reply with a number, or mix and match, for example "17 but warmer" or "16 with 11's skin".</p>
  <div class="grid">
{chr(10).join(cards)}
  </div>
</main>
</body></html>
'''
open(os.path.join(d, "index.html"), "w").write(page)
os.remove(os.path.join(d, "variants.json"))
print("page built,", len(m), "variants")
