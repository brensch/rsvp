"""Colour analysis + skin correction in CIELAB (D65), working in linear light for white balance.

usage: colour.py analyse <src.jpg>
       colour.py correct <src.jpg> <out.jpg> <params.json>
"""
import json, sys
import numpy as np
from PIL import Image

CROP = (497, 0, 497 + 1413, 1480)

# --- sRGB <-> linear <-> XYZ <-> Lab -------------------------------------------------------
M = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]])
MI = np.linalg.inv(M)
WHITE = np.array([0.95047, 1.0, 1.08883])

def to_lin(c):
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)

def to_srgb(c):
    c = np.clip(c, 0, 1)
    return np.where(c <= 0.0031308, c * 12.92, 1.055 * c ** (1 / 2.4) - 0.055)

def f(t):
    d = 6 / 29
    return np.where(t > d ** 3, np.cbrt(t), t / (3 * d * d) + 4 / 29)

def finv(t):
    d = 6 / 29
    return np.where(t > d, t ** 3, 3 * d * d * (t - 4 / 29))

def lin_to_lab(lin):
    xyz = lin @ M.T / WHITE
    fx, fy, fz = f(xyz[..., 0]), f(xyz[..., 1]), f(xyz[..., 2])
    return np.stack([116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)], -1)

def lab_to_lin(lab):
    fy = (lab[..., 0] + 16) / 116
    fx, fz = fy + lab[..., 1] / 500, fy - lab[..., 2] / 200
    xyz = np.stack([finv(fx), finv(fy), finv(fz)], -1) * WHITE
    return xyz @ MI.T

def load(path):
    im = Image.open(path).convert("RGB").crop(CROP)
    return np.asarray(im, dtype=np.float64) / 255.0

# Regions in crop pixel coordinates (x0, y0, x1, y1), picked from the 600px preview x 2.355.
REGIONS = {
    "brendan_cheek_L": (365, 450, 430, 520),
    "brendan_cheek_R": (515, 440, 575, 505),
    "brendan_forehead": (420, 300, 540, 350),
    "alyssa_cheek_L": (820, 380, 870, 440),
    "alyssa_cheek_R": (935, 380, 985, 440),
    "alyssa_forehead": (850, 250, 950, 290),
    "hands": (700, 1250, 860, 1360),
    "napkin_white": (760, 1060, 860, 1110),
    "tablecloth": (620, 1060, 700, 1130),
    "sky_cloud": (1150, 80, 1350, 200),
}

def stats(lab, box):
    x0, y0, x1, y1 = box
    p = lab[y0:y1, x0:x1].reshape(-1, 3)
    L, a, b = np.median(p, 0)
    return dict(L=round(L, 1), a=round(a, 1), b=round(b, 1), C=round(float(np.hypot(a, b)), 1), h=round(float(np.degrees(np.arctan2(b, a))), 1))

def skin_mask(lab):
    """Soft weight for skin-like colours: hue 15-75 deg (feathered), chroma 8-45, L 25-92."""
    L, a, b = lab[..., 0], lab[..., 1], lab[..., 2]
    C = np.hypot(a, b); h = np.degrees(np.arctan2(b, a))
    def ramp(x, lo0, lo1, hi1, hi0):
        return np.clip(np.minimum((x - lo0) / (lo1 - lo0), (hi0 - x) / (hi0 - hi1)), 0, 1)
    return ramp(h, 5, 22, 65, 82) * ramp(C, 4, 10, 42, 55) * ramp(L, 20, 30, 88, 96)

def analyse(path, title):
    lin = to_lin(load(path)); lab = lin_to_lab(lin)
    print(f"== {title}")
    for k, box in REGIONS.items():
        print(f"  {k:18s}", stats(lab, box))
    m = skin_mask(lab)
    print(f"  skin-mask coverage: {100 * (m > 0.5).mean():.1f}% of pixels")

def correct(src, out, params):
    srgb = load(src); lin = to_lin(srgb)
    # 1. White balance (von Kries in linear RGB), partial so the room keeps some warmth.
    wb = np.array(params.get("wb_gain", [1, 1, 1]))
    lin = lin * wb
    lab = lin_to_lab(lin)
    # 2. Skin-only: rotate hue toward target and trim chroma, weighted by the soft skin mask.
    w = skin_mask(lab)[..., None]
    L, a, b = lab[..., 0], lab[..., 1], lab[..., 2]
    C = np.hypot(a, b); h = np.arctan2(b, a)
    h2 = h + np.radians(params["hue_shift_deg"]) * w[..., 0]
    C2 = C * (1 - params["chroma_cut"] * w[..., 0])
    lab2 = np.stack([L, C2 * np.cos(h2), C2 * np.sin(h2)], -1)
    res = to_srgb(lab_to_lin(lab2))
    Image.fromarray((res * 255 + 0.5).astype(np.uint8)).save(out, quality=95, subsampling=0)
    # mask preview for inspection
    Image.fromarray((skin_mask(lab) * 255).astype(np.uint8)).resize((600, 628)).save(out.replace(".jpg", "-mask.png"))

if __name__ == "__main__":
    if sys.argv[1] == "analyse":
        analyse(sys.argv[2], sys.argv[2])
    else:
        correct(sys.argv[2], sys.argv[3], json.loads(sys.argv[4]))
