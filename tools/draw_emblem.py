#!/usr/bin/env python3
"""Draw the wedding emblem: golden wattle (Australia) and olive (USA) rising from a ribbon bow
into an open wreath, with room for the couple's initials in the middle.

    python3 tools/draw_emblem.py > templates/art/emblem.svg

Plain SVG, no dependencies. Randomness is seeded, so the drawing is the same every run.
"""
import math
import random

W, H = 600, 520
CX = W / 2
rnd = random.Random(9071927)

# Palette (matches the cards)
STEM_W = "#9a6a2c"      # wattle twigs, warm brown
LEAF_W = "#7f7a3c"      # wattle phyllodes, khaki
LEAF_W2 = "#969047"
GOLD = "#e3ac35"        # wattle balls
GOLD_HI = "#f6d77e"
GOLD_LO = "#c58d22"
BUD = "#d9d595"         # unopened wattle buds
STEM_O = "#7b6f57"      # olive wood, grey-brown
LEAF_O = "#68744f"      # olive leaf top
LEAF_O2 = "#a9b08f"     # olive leaf underside (silvery)
OLIVE = "#3a2836"       # ripe olive, deep aubergine
OLIVE_HI = "#7d5e74"
OLIVE_G = "#7d8540"     # green olive
RIBBON = "#c9a14f"
RIBBON_D = "#a7823a"

out = []


def f(x):
    return f"{x:.1f}"


def bez(p0, p1, p2, p3, t):
    u = 1 - t
    return (
        u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
        u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
    )


def bez_tangent(p0, p1, p2, p3, t):
    u = 1 - t
    dx = 3 * u * u * (p1[0] - p0[0]) + 6 * u * t * (p2[0] - p1[0]) + 3 * t * t * (p3[0] - p2[0])
    dy = 3 * u * u * (p1[1] - p0[1]) + 6 * u * t * (p2[1] - p1[1]) + 3 * t * t * (p3[1] - p2[1])
    return math.atan2(dy, dx)


def tapered_stem(p0, p1, p2, p3, w0, w1, color, steps=40):
    """A stem as a filled shape that narrows from w0 to w1."""
    left, right = [], []
    for i in range(steps + 1):
        t = i / steps
        x, y = bez(p0, p1, p2, p3, t)
        a = bez_tangent(p0, p1, p2, p3, t)
        w = (w0 + (w1 - w0) * t) / 2
        left.append((x + math.cos(a - math.pi / 2) * w, y + math.sin(a - math.pi / 2) * w))
        right.append((x + math.cos(a + math.pi / 2) * w, y + math.sin(a + math.pi / 2) * w))
    pts = left + right[::-1]
    d = "M" + " L".join(f"{f(x)},{f(y)}" for x, y in pts) + "Z"
    out.append(f'<path d="{d}" fill="{color}"/>')


def twig(x, y, ang, length, width, color, bend=0.25):
    """A short curved stem from (x, y) in direction ang. Returns its end point, end angle, and
    its curve (so things can be attached exactly on it)."""
    ex, ey = x + math.cos(ang) * length, y + math.sin(ang) * length
    nx, ny = math.cos(ang + math.pi / 2), math.sin(ang + math.pi / 2)
    c1 = (x + math.cos(ang) * length * 0.35 + nx * length * bend * 0.5, y + math.sin(ang) * length * 0.35 + ny * length * bend * 0.5)
    c2 = (x + math.cos(ang) * length * 0.7 + nx * length * bend, y + math.sin(ang) * length * 0.7 + ny * length * bend)
    out.append(
        f'<path d="M{f(x)},{f(y)} C{f(c1[0])},{f(c1[1])} {f(c2[0])},{f(c2[1])} {f(ex)},{f(ey)}" '
        f'fill="none" stroke="{color}" stroke-width="{width}" stroke-linecap="round"/>'
    )
    return (ex, ey), math.atan2(ey - c2[1], ex - c2[0]), ((x, y), c1, c2, (ex, ey))


def leaf(x, y, ang, length, width, fill, rib=None, curl=0.0, under=None):
    """A lance-shaped leaf with its base at (x, y), pointing along ang (radians).
    `curl` bends it sideways; `under` shows a sliver of a lighter underside."""
    L, Wd = length, width
    c = curl * L
    d = (
        f"M0,0 C{f(L*0.22)},{f(-Wd*1.05 + c*0.3)} {f(L*0.62)},{f(-Wd*0.95 + c*0.8)} {f(L)},{f(c)} "
        f"C{f(L*0.66)},{f(Wd*0.75 + c*0.8)} {f(L*0.25)},{f(Wd*0.9 + c*0.3)} 0,0Z"
    )
    g = f'<g transform="translate({f(x)},{f(y)}) rotate({f(math.degrees(ang))})">'
    if under:
        d2 = f"M0,0 C{f(L*0.25)},{f(Wd*0.9 + c*0.3)} {f(L*0.66)},{f(Wd*0.75 + c*0.8)} {f(L)},{f(c)} C{f(L*0.6)},{f(Wd*0.2 + c*0.7)} {f(L*0.25)},{f(Wd*0.25 + c*0.3)} 0,0Z"
        g += f'<path d="{d}" fill="{fill}"/><path d="{d2}" fill="{under}"/>'
    else:
        g += f'<path d="{d}" fill="{fill}"/>'
    if rib:
        g += f'<path d="M{f(L*0.04)},0 Q{f(L*0.5)},{f(c*0.5)} {f(L*0.9)},{f(c*0.9)}" fill="none" stroke="{rib}" stroke-width="0.8" stroke-linecap="round" opacity="0.7"/>'
    g += "</g>"
    out.append(g)


def ball(x, y, r):
    """A fluffy wattle flower: a gold ball with a soft highlight and a few stamen dots."""
    # a fringe of tiny stamen tips around the edge makes it fluffy rather than a bead
    for j in range(14):
        a = j / 14 * 2 * math.pi + rnd.uniform(-0.1, 0.1)
        rr = r * rnd.uniform(0.92, 1.12)
        out.append(f'<circle cx="{f(x + math.cos(a)*rr)}" cy="{f(y + math.sin(a)*rr)}" r="{f(r*0.2)}" fill="{GOLD if j % 3 else GOLD_LO}"/>')
    out.append(f'<circle cx="{f(x)}" cy="{f(y)}" r="{f(r*0.9)}" fill="url(#wattle)"/>')
    for _ in range(4):
        a = rnd.uniform(0, 2 * math.pi)
        rr = r * rnd.uniform(0.3, 0.7)
        out.append(f'<circle cx="{f(x + math.cos(a)*rr)}" cy="{f(y + math.sin(a)*rr)}" r="{f(r*0.16)}" fill="{GOLD_HI}" opacity="0.9"/>')


def bud(x, y, r):
    out.append(f'<circle cx="{f(x)}" cy="{f(y)}" r="{f(r)}" fill="{BUD}" stroke="#b9b56f" stroke-width="0.5"/>')


def olive(x, y, size, ripe=True, lean=0.0):
    """An olive hanging from (x, y): a short curved stalk, then an egg-shaped fruit (wider
    at the bottom), a tiny cap where it joins, and a soft highlight."""
    L = size * 1.25  # stalk
    tx, ty = x + math.sin(lean) * L, y + math.cos(lean) * L
    out.append(
        f'<path d="M{f(x)},{f(y)} Q{f(x + math.sin(lean)*L*0.2 + lean*4)},{f(y + L*0.6)} {f(tx)},{f(ty)}" '
        f'fill="none" stroke="{STEM_O}" stroke-width="0.9" stroke-linecap="round"/>'
    )
    rx, ry = size * 0.66, size
    egg = (
        f"M0,0 C{f(rx*0.75)},0 {f(rx*1.05)},{f(ry*0.9)} {f(rx*0.85)},{f(ry*1.45)} "
        f"C{f(rx*0.6)},{f(ry*2.02)} {f(-rx*0.6)},{f(ry*2.02)} {f(-rx*0.85)},{f(ry*1.45)} "
        f"C{f(-rx*1.05)},{f(ry*0.9)} {f(-rx*0.75)},0 0,0Z"
    )
    fill = "url(#olive)" if ripe else "url(#oliveg)"
    out.append(
        f'<g transform="translate({f(tx)},{f(ty)}) rotate({f(-math.degrees(lean) * 0.85)})">'
        f'<path d="{egg}" fill="{fill}"/>'
        f'<ellipse cx="0" cy="{f(ry*0.08)}" rx="{f(rx*0.32)}" ry="{f(ry*0.12)}" fill="{STEM_O}"/>'
        f'<ellipse cx="{f(-rx*0.38)}" cy="{f(ry*0.8)}" rx="{f(rx*0.2)}" ry="{f(ry*0.36)}" fill="#ffffff" opacity="0.3"/>'
        "</g>"
    )


# ---------------------------------------------------------------------------
# The two arcs. Each rises from just above the bow and curls inward near the top.

BASE_Y = 430


def arc(side):
    s = -1 if side == "left" else 1
    p0 = (CX + s * 6, BASE_Y)
    p1 = (CX + s * 150, BASE_Y + 8)
    p2 = (CX + s * 250, 270)
    p3 = (CX + s * 175, 62)
    return p0, p1, p2, p3


def wattle():
    p = arc("left")
    tapered_stem(*p, 5.0, 1.4, STEM_W)
    n = 15
    for i in range(n):
        t = 0.08 + 0.88 * (i + 0.5) / (n - 1)
        if t > 0.97:
            continue
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        k = 1 - t * 0.35
        out_ang = a - math.pi / 2  # outward (left of travel = outside of the arc)
        in_ang = a + math.pi / 2
        # flower racemes: a short twig crowded with balls, on the outside, and every other
        # node on the inside too
        sides = [in_ang if i % 2 == 0 else out_ang]
        for sa in sides:
            ang = sa * 0.6 + a * 0.4 if abs(sa - a) < math.pi else sa
            ang = math.atan2(math.sin(sa) * 0.6 + math.sin(a) * 0.4, math.cos(sa) * 0.6 + math.cos(a) * 0.4)
            inside = sa is in_ang
            tl = rnd.uniform(16, 22) * k * (0.8 if inside else 1)
            (ex, ey), ea, curve = twig(x, y, ang, tl, 1.3, STEM_W, bend=rnd.uniform(-0.25, 0.25))
            nb = rnd.randint(4, 5)
            for j in range(nb):
                tt = 0.25 + 0.75 * j / max(1, nb - 1)
                bx, by = bez(*curve, tt)  # on the twig itself, not the straight line between its ends
                side = 1 if j % 2 else -1
                px = bx + math.cos(ang + side * 1.2) * 6.5 * k
                py = by + math.sin(ang + side * 1.2) * 6.5 * k
                out.append(f'<path d="M{f(bx)},{f(by)} L{f(px)},{f(py)}" stroke="{STEM_W}" stroke-width="0.7"/>')
                if rnd.random() < 0.15:
                    bud(px, py, 2.6 * k)
                else:
                    ball(px, py, rnd.uniform(4.6, 5.5) * k)
            ball(ex + math.cos(ea) * 4, ey + math.sin(ea) * 4, 5.4 * k)
    # phyllodes on top of the flowers so they show: slender, curved, alternating out and in,
    # pointing toward the tip
    for i in range(n):
        t = 0.08 + 0.88 * i / (n - 1)
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        k = 1 - t * 0.35
        if i % 2 == 0:
            leaf(x, y, a - 0.62 + rnd.uniform(-0.1, 0.1), 56 * k, 5.6 * k, LEAF_W, rib="#5f5a2a", curl=-0.07)
        else:
            leaf(x, y, a + 0.55 + rnd.uniform(-0.1, 0.1), 48 * k, 5.0 * k, LEAF_W2, rib="#6d6830", curl=0.07)
    # a cluster at the very tip
    x, y = p[3]
    for dx, dy, r in ((0, -5, 4.8), (-7, 2, 4.4), (6, 2, 4.2), (-2, -13, 4.0)):
        ball(x + dx, y + dy, r)


def olive_branch():
    p = arc("right")
    tapered_stem(*p, 4.6, 1.3, STEM_O)
    n = 15
    nodes = []
    for i in range(n):
        t = 0.06 + 0.92 * i / (n - 1)
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        nodes.append((i, t, x, y, a, 1 - t * 0.3))
    # olive leaves come in opposite pairs, angled up toward the tip
    for i, t, x, y, a, k in nodes:
        for side in (-1, 1):
            if i == n - 1 and side == 1:
                continue
            la = a + side * (0.6 + rnd.uniform(-0.08, 0.08))
            leaf(x, y, la, 52 * k, 6.0 * k, LEAF_O, rib="#4f593b", curl=side * 0.04,
                 under=LEAF_O2 if side == (1 if i % 2 else -1) else None)
    # fruit on top of the leaves: one or two olives at most nodes,
    # hanging down from the leaf joints, alternating outside and inside of the arc
    for i, t, x, y, a, k in nodes:
        if i in (0, n - 1) or i % 3 == 2:
            continue
        side = 1 if i % 2 else -1
        ox, oy = x, y  # stalks start on the branch's centre line, so they always join it
        # hang mostly downward, angled a little away from the branch so the fruit clears it
        # (olive() measures `lean` from straight down)
        n = a + side * math.pi / 2
        hx, hy = math.cos(n), math.sin(n) + 1.6
        lean = math.pi / 2 - math.atan2(hy, hx) + rnd.uniform(-0.1, 0.1)
        if i % 4 == 0:  # a pair, fanned apart
            olive(ox, oy, 8.6 * k, ripe=True, lean=lean + side * 0.45)
            olive(ox, oy, 9.6 * k, ripe=True, lean=lean - side * 0.2)
        else:
            olive(ox, oy, 9.6 * k, ripe=(i % 4 != 1), lean=lean)
    # tip: a final leaf pointing on
    x, y = p[3]
    a = bez_tangent(*p, 1)
    leaf(x, y, a, 48, 6.0, LEAF_O, rib="#4f593b")


def bow():
    x, y = CX, BASE_Y + 4
    # tails
    for s in (-1, 1):
        out.append(
            f'<path d="M{f(x)},{f(y)} C{f(x+s*14)},{f(y+26)} {f(x+s*8)},{f(y+44)} {f(x+s*30)},{f(y+70)} '
            f'L{f(x+s*38)},{f(y+60)} L{f(x+s*40)},{f(y+74)} C{f(x+s*18)},{f(y+48)} {f(x+s*20)},{f(y+26)} {f(x+s*4)},{f(y+4)}Z" '
            f'fill="{RIBBON}" stroke="{RIBBON_D}" stroke-width="0.8"/>'
        )
    # loops: an outer ribbon edge, the darker inside of the loop showing through its opening,
    # and a fold where the ribbon turns
    for s in (-1, 1):
        out.append(
            f'<path d="M{f(x)},{f(y)} C{f(x+s*14)},{f(y-34)} {f(x+s*58)},{f(y-34)} {f(x+s*52)},{f(y-6)} '
            f'C{f(x+s*48)},{f(y+14)} {f(x+s*20)},{f(y+12)} {f(x)},{f(y)}Z" fill="{RIBBON}" stroke="{RIBBON_D}" stroke-width="0.9"/>'
        )
        out.append(
            f'<path d="M{f(x+s*10)},{f(y-4)} C{f(x+s*18)},{f(y-24)} {f(x+s*44)},{f(y-24)} {f(x+s*42)},{f(y-8)} '
            f'C{f(x+s*40)},{f(y+2)} {f(x+s*22)},{f(y+2)} {f(x+s*10)},{f(y-4)}Z" fill="{RIBBON_D}" opacity="0.55"/>'
        )
        out.append(
            f'<path d="M{f(x+s*44)},{f(y-20)} C{f(x+s*52)},{f(y-14)} {f(x+s*52)},{f(y-4)} {f(x+s*46)},{f(y+4)}" '
            f'fill="none" stroke="#e6c77c" stroke-width="1.6" stroke-linecap="round" opacity="0.8"/>'
        )
    # knot
    out.append(f'<ellipse cx="{f(x)}" cy="{f(y)}" rx="8" ry="7" fill="{RIBBON}" stroke="{RIBBON_D}" stroke-width="0.8"/>')


wattle()
olive_branch()
bow()

print(
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">\n'
    "<!-- Wedding emblem: golden wattle and olive. Drawn by tools/draw_emblem.py. -->\n"
    "<defs>"
    f'<radialGradient id="wattle" cx="0.38" cy="0.35" r="0.7"><stop offset="0" stop-color="{GOLD_HI}"/><stop offset="0.55" stop-color="{GOLD}"/><stop offset="1" stop-color="{GOLD_LO}"/></radialGradient>'
    f'<radialGradient id="olive" cx="0.35" cy="0.3" r="0.8"><stop offset="0" stop-color="{OLIVE_HI}"/><stop offset="1" stop-color="{OLIVE}"/></radialGradient>'
    f'<radialGradient id="oliveg" cx="0.35" cy="0.3" r="0.8"><stop offset="0" stop-color="#aab06e"/><stop offset="1" stop-color="{OLIVE_G}"/></radialGradient>'
    "</defs>\n"
    # Mirrored so the olive (USA) sits on the left under A for Alyssa, and the wattle
    # (Australia) on the right under B for Brend.
    + f'<g transform="translate({W},0) scale(-1,1)">\n'
    + "\n".join(out)
    + "\n</g>\n</svg>"
)
