#!/usr/bin/env python3
"""Draw the wedding emblem: golden wattle (Australia) and California poppies (USA) rising from a ribbon bow
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
STEM_P = "#7d9585"      # poppy stems, glaucous blue-green
FERN = "#7f9a8a"        # poppy foliage, finely divided
FERN2 = "#a3b5a7"
POPPY = "#ec8420"       # California poppy orange
POPPY_LIGHT = "#f6b443"
POPPY_DARK = "#d26614"
POPPY_CENTRE = "#c58a1f"
RIBBON = "#a0702f"     # bow: caramel brown, the darker shade of both the gold wattle and orange poppies
RIBBON_D = "#6f4b1f"

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


def petal(ang, length, width, fill, veins=True):
    """One broad, fan-shaped poppy petal from the flower's centre, pointing along ang."""
    L, Wd = length, width / 2
    d = (
        f"M0,0 C{f(L*0.2)},{f(-Wd*0.5)} {f(L*0.62)},{f(-Wd*1.08)} {f(L*0.92)},{f(-Wd*0.62)} "
        f"Q{f(L*1.08)},0 {f(L*0.92)},{f(Wd*0.62)} C{f(L*0.62)},{f(Wd*1.08)} {f(L*0.2)},{f(Wd*0.5)} 0,0Z"
    )
    g = f'<g transform="rotate({f(math.degrees(ang))})"><path d="{d}" fill="{fill}"/>'
    if veins:
        for v in (-0.35, 0, 0.35):
            g += f'<path d="M{f(L*0.15)},0 Q{f(L*0.55)},{f(v*Wd*0.6)} {f(L*0.85)},{f(v*Wd*1.0)}" fill="none" stroke="{POPPY_DARK}" stroke-width="0.5" opacity="0.45"/>'
    out.append(g + "</g>")


def poppy(x, y, size, facing):
    """An open California poppy: four broad petals (the back pair a shade deeper), a golden
    base, and a ring of stamens. `facing` tilts the flower toward its stalk's direction."""
    out.append(f'<g transform="translate({f(x)},{f(y)}) rotate({f(math.degrees(facing))}) scale(1,0.86)">')
    base = rnd.uniform(0, 0.4)
    for j, k in enumerate((0, 2, 1, 3)):  # back pair first, front pair on top
        a = base + k * math.pi / 2 + rnd.uniform(-0.12, 0.12)
        petal(a, size * rnd.uniform(0.96, 1.06), size * 1.25, "url(#poppyBack)" if j < 2 else "url(#poppyFront)")
    out.append(f'<circle r="{f(size*0.24)}" fill="{POPPY_CENTRE}"/>')
    for j in range(12):
        a = j / 12 * 2 * math.pi
        out.append(f'<circle cx="{f(math.cos(a)*size*0.22)}" cy="{f(math.sin(a)*size*0.22)}" r="{f(size*0.06)}" fill="#f3cf6a"/>')
    out.append("</g>")


def poppy_bud(x, y, ang, size):
    """A slender pointed bud, the orange petals just showing at the tip."""
    L, Wd = size * 2.2, size * 0.55
    d = f"M0,0 C{f(L*0.1)},{f(-Wd)} {f(L*0.7)},{f(-Wd*0.9)} {f(L)},0 C{f(L*0.7)},{f(Wd*0.9)} {f(L*0.1)},{f(Wd)} 0,0Z"
    tip = f"M{f(L*0.62)},{f(-Wd*0.55)} Q{f(L*0.85)},{f(-Wd*0.3)} {f(L)},0 Q{f(L*0.85)},{f(Wd*0.3)} {f(L*0.62)},{f(Wd*0.55)} Q{f(L*0.7)},0 {f(L*0.62)},{f(-Wd*0.55)}Z"
    out.append(f'<g transform="translate({f(x)},{f(y)}) rotate({f(math.degrees(ang))})"><path d="{d}" fill="{FERN}"/><path d="{tip}" fill="{POPPY}"/></g>')


def fern(x, y, ang, length, color, bend=0.18):
    """A finely divided poppy leaf: a gently curved stalk with fine, forked, curving segments."""
    nx, ny = math.cos(ang + math.pi / 2), math.sin(ang + math.pi / 2)
    ex, ey = x + math.cos(ang) * length, y + math.sin(ang) * length
    cx, cy = x + math.cos(ang) * length * 0.5 + nx * length * bend, y + math.sin(ang) * length * 0.5 + ny * length * bend

    def on(t):  # point and direction along the stalk (a quadratic curve)
        px = (1 - t) ** 2 * x + 2 * (1 - t) * t * cx + t * t * ex
        py = (1 - t) ** 2 * y + 2 * (1 - t) * t * cy + t * t * ey
        dx = 2 * (1 - t) * (cx - x) + 2 * t * (ex - cx)
        dy = 2 * (1 - t) * (cy - y) + 2 * t * (ey - cy)
        return px, py, math.atan2(dy, dx)

    def stroke(x0, y0, a, ln, w, curl):
        x1, y1 = x0 + math.cos(a) * ln, y0 + math.sin(a) * ln
        qx = x0 + math.cos(a) * ln * 0.5 + math.cos(a + math.pi / 2) * ln * curl
        qy = y0 + math.sin(a) * ln * 0.5 + math.sin(a + math.pi / 2) * ln * curl
        out.append(f'<path d="M{f(x0)},{f(y0)} Q{f(qx)},{f(qy)} {f(x1)},{f(y1)}" fill="none" stroke="{color}" stroke-width="{w}" stroke-linecap="round"/>')
        return x1, y1

    out.append(f'<path d="M{f(x)},{f(y)} Q{f(cx)},{f(cy)} {f(ex)},{f(ey)}" fill="none" stroke="{color}" stroke-width="1.0" stroke-linecap="round"/>')
    for i in range(5):
        t = 0.25 + 0.75 * i / 4
        px, py, d = on(t)
        seg = length * (0.42 - 0.22 * t)
        for side in (-1, 1):
            a = d + side * (0.8 - 0.3 * t)
            qx, qy = stroke(px, py, a, seg, 0.95, side * 0.12)
            for fork in (-0.45, 0.35):  # each segment splits into fine tips
                stroke(qx, qy, a + side * fork, seg * 0.5, 0.75, side * 0.15)
    px, py, d = on(1.0)
    stroke(px, py, d, length * 0.18, 0.85, 0.1)


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


def poppies():
    p = arc("right")
    tapered_stem(*p, 4.2, 1.3, STEM_P)
    n = 14

    def outward(x, y, a):
        """The side normal that points away from the middle of the wreath."""
        nrm = a + math.pi / 2
        return nrm if math.cos(nrm) * (x - CX) + math.sin(nrm) * (y - 250) > 0 else nrm + math.pi

    # feathery leaves all along, alternating sides
    for i in range(n):
        t = 0.06 + 0.9 * i / (n - 1)
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        k = 1 - t * 0.3
        o = outward(x, y, a)
        side_ang = o if i % 2 == 0 else o + math.pi
        fern(x, y, math.atan2(math.sin(side_ang) * 0.55 + math.sin(a) * 0.45, math.cos(side_ang) * 0.55 + math.cos(a) * 0.45),
             34 * k, FERN if i % 2 else FERN2, bend=0.15 if i % 2 else -0.15)
    def side_dir(x, y, a, outside, lean):
        """Out from the stem on the outside or inside of the wreath, leaning toward the tip."""
        n = outward(x, y, a) if outside else outward(x, y, a) + math.pi
        return math.atan2(math.sin(n) * lean + math.sin(a) * (1 - lean), math.cos(n) * lean + math.cos(a) * (1 - lean))

    # buds and flowers alternate sides: flowers outside, inside, outside...; each bud sits on
    # the opposite side to the flowers around it
    for j, t in enumerate((0.24, 0.45, 0.65, 0.84)):
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        (ex, ey), ea, _ = twig(x, y, side_dir(x, y, a, j % 2 == 0, 0.5), 14, 1.2, STEM_P, bend=0.2)
        poppy_bud(ex, ey, ea, 8.5 * (1 - t * 0.25))
    # flowers each on their own stalk, smaller toward the tip and a touch smaller inside the wreath
    for j, (t, size) in enumerate(((0.12, 24), (0.33, 23), (0.54, 21), (0.74, 19), (0.93, 16))):
        x, y = bez(*p, t)
        a = bez_tangent(*p, t)
        outside = j % 2 == 0
        size = size if outside else size * 0.88
        (ex, ey), ea, _ = twig(x, y, side_dir(x, y, a, outside, 0.6), 18 if outside else 14, 1.5, STEM_P, bend=-0.15 if outside else 0.15)
        poppy(ex + math.cos(ea) * size * 0.3, ey + math.sin(ea) * size * 0.3, size, ea)
    # tip: a last bud
    x, y = p[3]
    poppy_bud(x, y, bez_tangent(*p, 1), 7.5)


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
            f'fill="none" stroke="#d4a764" stroke-width="1.6" stroke-linecap="round" opacity="0.8"/>'
        )
    # knot
    out.append(f'<ellipse cx="{f(x)}" cy="{f(y)}" rx="8" ry="7" fill="{RIBBON}" stroke="{RIBBON_D}" stroke-width="0.8"/>')


wattle()
poppies()
bow()

print(
    f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}">\n'
    "<!-- Wedding emblem: golden wattle and California poppies. Drawn by tools/draw_emblem.py. -->\n"
    "<defs>"
    f'<radialGradient id="wattle" cx="0.38" cy="0.35" r="0.7"><stop offset="0" stop-color="{GOLD_HI}"/><stop offset="0.55" stop-color="{GOLD}"/><stop offset="1" stop-color="{GOLD_LO}"/></radialGradient>'
    # California poppy petals: deep red-orange at the base, through orange, to golden tips
    f'<linearGradient id="poppyFront" x1="0" y1="0" x2="1" y2="0"><stop offset="0.2" stop-color="#c23b16"/><stop offset="0.45" stop-color="#e5671c"/><stop offset="0.75" stop-color="#f29a2e"/><stop offset="1" stop-color="#fbd462"/></linearGradient>'
    f'<linearGradient id="poppyBack" x1="0" y1="0" x2="1" y2="0"><stop offset="0.2" stop-color="#a93214"/><stop offset="0.5" stop-color="#d65a19"/><stop offset="0.8" stop-color="#ea8a2a"/><stop offset="1" stop-color="#f4bb4a"/></linearGradient>'
    "</defs>\n"
    # Mirrored so the poppies (USA) sit on the left under the American partner's initial, and the
    # wattle (Australia) on the right under the Australian partner's.
    + f'<g transform="translate({W},0) scale(-1,1)">\n'
    + "\n".join(out)
    + "\n</g>\n</svg>"
)
