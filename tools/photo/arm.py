"""Even out the shadow on Alyssa's arm with frequency separation in CIELAB.

usage: arm.py <in.jpg> <out.jpg> <strength 0-1>
"""
import sys, importlib.util
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

spec = importlib.util.spec_from_file_location("c", "colour.py"); c = importlib.util.module_from_spec(spec); spec.loader.exec_module(c)

src, out, k = sys.argv[1], sys.argv[2], float(sys.argv[3])
rgb = np.asarray(Image.open(src).convert("RGB"), dtype=np.float64) / 255
H, W = rgb.shape[:2]
lab = c.lin_to_lab(c.to_lin(rgb))

# Her left arm (image right): shoulder -> elbow -> forearm, stopping at the bracelet.
ARM = [(1120, 600), (1200, 585), (1240, 640), (1265, 800), (1290, 950), (1310, 1080), (1300, 1180), (1240, 1225),
       (1100, 1262), (950, 1300), (930, 1240), (960, 1212), (1120, 1165), (1200, 1110), (1160, 1000), (1130, 850), (1115, 700)]
poly = Image.new("L", (W, H), 0); ImageDraw.Draw(poly).polygon(ARM, fill=255)
poly = np.asarray(poly.filter(ImageFilter.GaussianBlur(4)), dtype=np.float64) / 255
skin = c.skin_mask(lab)
skin = np.asarray(Image.fromarray((skin * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(2)), dtype=np.float64) / 255
mask = poly * skin  # 0..1, soft at the arm's edges and around dress dots

def _box(x, r, axis):
    """Box filter of radius r along an axis (edge-padded), via cumulative sums."""
    pad = [(0, 0)] * x.ndim; pad[axis] = (r + 1, r)
    cs = np.cumsum(np.pad(x, pad, mode="edge"), axis=axis)
    n = x.shape[axis]
    return (np.take(cs, np.arange(2 * r + 1, 2 * r + 1 + n), axis=axis) - np.take(cs, np.arange(0, n), axis=axis)) / (2 * r + 1)

def blur(x, sigma):
    """Gaussian approximated by three box passes per axis (radius from sigma)."""
    r = max(1, int(round((np.sqrt(12 * sigma * sigma / 3 + 1) - 1) / 2)))
    for _ in range(3):
        x = _box(_box(x, r, 0), r, 1)
    return x

def masked_blur(ch, r):
    """Normalised convolution: average of arm pixels only, so the dress never bleeds in."""
    num = blur(ch * mask, r); den = blur(mask, r)
    return num / np.maximum(den, 1e-4)

out_lab = lab.copy()
# Broad light (r=18) vs the arm's overall lighting (r=90): the difference is the shadow to lift.
for i, (gain_up, gain_down) in enumerate([(k, k * 0.25), (k * 0.8, k * 0.8), (k * 0.8, k * 0.8)]):
    low = masked_blur(lab[..., i], 18)
    broad = masked_blur(lab[..., i], 90)
    d = broad - low
    # Lift shadows fully (by k); only gently pull down highlights, so the arm keeps its roundness.
    adj = np.where(d > 0, d * gain_up, d * gain_down) if i == 0 else d * gain_up
    out_lab[..., i] = lab[..., i] + adj * mask

res = c.to_srgb(c.lab_to_lin(out_lab))
Image.fromarray((res * 255 + 0.5).astype(np.uint8)).save(out, quality=95, subsampling=0)
Image.fromarray((mask * 255).astype(np.uint8)).save(out.replace(".jpg", "-mask.png"))

# Report: lightness/hue along the arm before vs after
def st(L, box):
    x0, y0, x1, y1 = box; m = mask[y0:y1, x0:x1] > 0.6
    p = L[y0:y1, x0:x1][m]
    return np.median(p, 0) if len(p) else None
for name, box in {"shoulder (lit)": (1150, 610, 1210, 680), "upper arm inner (shadow)": (1135, 800, 1175, 950),
                  "upper arm outer": (1210, 800, 1255, 950), "elbow": (1230, 1080, 1290, 1160), "forearm": (1000, 1215, 1120, 1260)}.items():
    a, b = st(lab, box), st(out_lab, box)
    if a is None: print(name, "no skin"); continue
    print(f"{name:26s} L {a[0]:.0f} -> {b[0]:.0f}   hue {np.degrees(np.arctan2(a[2], a[1])):.0f} -> {np.degrees(np.arctan2(b[2], b[1])):.0f}")
