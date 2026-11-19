"""Numbered edit variants of the home photo, for a comparison page.

usage: variants.py <edited.jpg> <unedited_crop.jpg> <outdir>
Writes <outdir>/NN.jpg (display, 1000px wide), <outdir>/NN-full.jpg and <outdir>/variants.json.
"""
import json, os, sys, importlib.util
import numpy as np
from PIL import Image

spec = importlib.util.spec_from_file_location("c", os.path.join(os.path.dirname(__file__), "colour.py"))
c = importlib.util.module_from_spec(spec); spec.loader.exec_module(c)

base_path, orig_path, outdir = sys.argv[1:4]
os.makedirs(outdir, exist_ok=True)

def load(p):
    return np.asarray(Image.open(p).convert("RGB"), dtype=np.float64) / 255

def _box(x, r, axis):
    pad = [(0, 0)] * x.ndim; pad[axis] = (r + 1, r)
    cs = np.cumsum(np.pad(x, pad, mode="edge"), axis=axis)
    n = x.shape[axis]
    return (np.take(cs, np.arange(2 * r + 1, 2 * r + 1 + n), axis=axis) - np.take(cs, np.arange(0, n), axis=axis)) / (2 * r + 1)

def blur(x, sigma):
    r = max(1, int(round((np.sqrt(12 * sigma * sigma / 3 + 1) - 1) / 2)))
    for _ in range(3):
        x = _box(_box(x, r, 0), r, 1)
    return x

BASE = load(base_path)
H, W = BASE.shape[:2]

# ---- building blocks (all on a Lab image unless noted) ----------------------------------------
def lab_of(rgb): return c.lin_to_lab(c.to_lin(rgb))
def rgb_of(lab): return c.to_srgb(c.lab_to_lin(lab))
def skin(lab): return c.skin_mask(lab)

def smoothstep(e0, e1, x):
    t = np.clip((x - e0) / (e1 - e0), 0, 1); return t * t * (3 - 2 * t)

def s_curve(lab, amount):
    """Contrast around L=50 with a smooth S (keeps black and white points)."""
    L = lab[..., 0] / 100
    s = L + amount * np.sin(2 * np.pi * L) * -0.5 / np.pi * 2  # pushes mids apart, endpoints fixed
    out = lab.copy(); out[..., 0] = np.clip(s, 0, 1) * 100; return out

def exposure(rgb, ev):
    return c.to_srgb(c.to_lin(rgb) * 2 ** ev)

def lift_shadows(lab, amount, pivot=35):
    L = lab[..., 0]; w = 1 - smoothstep(0, pivot, L)
    out = lab.copy(); out[..., 0] = L + amount * w * (pivot - L) * 0.5; return out

def recover_highlights(lab, amount, start=72):
    """Local: pull bright areas down relative to their surroundings, keeping texture."""
    L = lab[..., 0]; low = blur(L, 25); w = smoothstep(start, 95, low)
    out = lab.copy(); out[..., 0] = L - amount * w * (low - start) * 0.6; return out

def vibrance(lab, amount, protect_skin=0.8):
    C = np.hypot(lab[..., 1], lab[..., 2]); boost = 1 + amount * np.clip(1 - C / 45, 0, 1) * (1 - protect_skin * skin(lab))
    out = lab.copy(); out[..., 1] *= boost; out[..., 2] *= boost; return out

def saturation(lab, factor):
    out = lab.copy(); out[..., 1] *= factor; out[..., 2] *= factor; return out

def white_balance(rgb, gains):
    return c.to_srgb(c.to_lin(rgb) * np.array(gains))

def clarity(lab, amount, sigma=30, skin_protect=0.7):
    L = lab[..., 0]; detail = L - blur(L, sigma)
    mid = smoothstep(10, 35, L) * (1 - smoothstep(70, 92, L))
    out = lab.copy(); out[..., 0] = L + amount * detail * mid * (1 - skin_protect * skin(lab)); return out

def sharpen(lab, amount, sigma=1.1, threshold=1.0):
    L = lab[..., 0]; d = L - blur(L, sigma); d = np.sign(d) * np.maximum(np.abs(d) - threshold, 0)
    out = lab.copy(); out[..., 0] = L + amount * d; return out

def smooth_skin(lab, amount):
    """Frequency separation on skin: soften the mid band (blotches), keep fine texture (pores)."""
    L = lab[..., 0]; fine = L - blur(L, 1.5); mid = blur(L, 1.5) - blur(L, 8)
    w = skin(lab) * amount
    out = lab.copy(); out[..., 0] = L - w * mid * 0.6
    for i in (1, 2):
        ch = lab[..., i]; out[..., i] = ch - w * (ch - blur(ch, 4)) * 0.5
    return out

def vignette(rgb, amount):
    yy, xx = np.mgrid[0:H, 0:W]; cx, cy = W * 0.5, H * 0.42
    r = np.hypot((xx - cx) / (W * 0.62), (yy - cy) / (H * 0.62))
    v = 1 - amount * smoothstep(0.55, 1.25, r)
    return c.to_srgb(c.to_lin(rgb) * v[..., None])

def matte(lab, lift=6, fade=0.9):
    out = lab.copy(); out[..., 0] = lift + lab[..., 0] * (100 - lift) / 100
    out[..., 1] *= fade; out[..., 2] *= fade; return out

def mono(lab, contrast=0.15):
    out = s_curve(lab, contrast); out[..., 1] = 0; out[..., 2] = 0; return out

def warm_tone_bw(lab):
    out = mono(lab, 0.12); out[..., 1] = 1.5; out[..., 2] = 6.0; return out

def L(rgb): return lab_of(rgb)
def R(lab): return rgb_of(lab)

# ---- the variants ------------------------------------------------------------------------------
V = []
def add(title, note, rgb): V.append((title, note, np.clip(rgb, 0, 1)))

add("Unedited crop", "For reference: the photo before any colour work.", load(orig_path))
add("Current", "What's on the site now: white balance, skin warmth softened, arm shadow evened.", BASE)
add("Gentle contrast", "A soft S-curve: a little more depth without crushing the suit or the dress.", R(s_curve(L(BASE), 0.10)))
add("Brighter and airier", "Up a third of a stop with lifted shadows. Lighter and more 'wedding'.", R(lift_shadows(L(exposure(BASE, 0.33)), 0.5)))
add("Window recovered", "Brings back detail in the bright window and sky so the city reads better.", R(recover_highlights(L(BASE), 0.8)))
add("Shadows opened", "Lifts the dark suit, jacket and corners so nothing goes to black.", R(lift_shadows(L(BASE), 0.9, 40)))
add("Vibrance", "Richer navy, city and greenery, with skin protected so it doesn't go red again.", R(vibrance(L(BASE), 0.35)))
add("Warm evening", "Slightly warmer overall, like golden hour. Romantic, still natural skin.", white_balance(BASE, [1.035, 1.0, 0.95]))
add("Clean neutral", "A touch cooler and cleaner: whites read whiter, skin stays healthy.", white_balance(BASE, [0.97, 1.0, 1.035]))
add("Clarity", "Local contrast in the mid-tones: crisper dress, hair and city. Skin left soft.", R(clarity(L(BASE), 0.35)))
add("Sharpened", "Fine sharpening for faces, eyes, hair and the ring. Subtle at phone size.", R(sharpen(L(BASE), 0.8)))
add("Softer skin", "Gentle frequency-separation smoothing: evens blotches, keeps pores and freckles.", R(smooth_skin(L(BASE), 0.6)))
add("Vignette", "A barely-there darkening of the corners to draw the eye to your faces.", vignette(BASE, 0.22))
add("Soft matte", "Lifted blacks and slightly muted colour: a film-like, editorial wedding look.", R(matte(L(BASE), 6, 0.9)))
add("Black and white", "Classic mono with gentle contrast.", R(mono(L(BASE), 0.15)))
add("Warm black and white", "Mono with a faint warm (sepia-leaning) tone.", R(warm_tone_bw(L(BASE))))

polish = R(sharpen(clarity(vibrance(s_curve(recover_highlights(L(BASE), 0.6), 0.07), 0.25), 0.25), 0.6))
add("Polished (recommended)", "Window recovered, gentle contrast, vibrance, clarity and sharpening together.", polish)
add("Polished + vignette", "The polished version with a subtle vignette.", vignette(polish, 0.18))
bright_polish = R(sharpen(vibrance(lift_shadows(recover_highlights(L(exposure(BASE, 0.25)), 0.8), 0.5), 0.25), 0.6))
add("Bright and polished", "Brighter and airier, window recovered, vibrance and sharpening.", bright_polish)
add("Polished, softer skin", "Polished plus gentle skin smoothing.", R(smooth_skin(L(polish), 0.5)))
add("Warm polished", "Polished with the warm evening grade.", white_balance(polish, [1.03, 1.0, 0.955]))
add("Editorial", "Soft matte, gentle contrast, clarity and vignette: magazine-style.", vignette(R(matte(clarity(s_curve(L(BASE), 0.08), 0.25), 5, 0.88)), 0.15))

meta = []
for i, (title, note, rgb) in enumerate(V):
    n = f"{i:02d}"
    im = Image.fromarray((rgb * 255 + 0.5).astype(np.uint8))
    im.save(os.path.join(outdir, f"{n}-full.jpg"), quality=93, subsampling=0)
    im.resize((1000, round(1000 * H / W)), Image.LANCZOS).save(os.path.join(outdir, f"{n}.jpg"), quality=86, optimize=True, progressive=True)
    meta.append({"n": i, "file": n, "title": title, "note": note})
json.dump(meta, open(os.path.join(outdir, "variants.json"), "w"), indent=1)
print(len(meta), "variants")
