# Home photo edits

How the save-the-date photo (PXL_20260503_033839546.jpg) was edited. Needs numpy and Pillow.
Run from this folder (the scripts load `colour.py` beside them).

1. `colour.py correct <src> v1.jpg '{"wb_gain":[0.849,1,0.990],"hue_shift_deg":10,"chroma_cut":0.08}'`
   crops (CROP), partially white-balances against the napkin, and moves skin hue from ~27-38 deg to
   ~47-52 deg (CIELAB) through a soft skin mask. `colour.py analyse <src>` prints the measurements.
2. `arm.py v1.jpg arm.jpg 0.85` evens the shadow on Alyssa's upper arm with frequency separation
   (lifts the broad shadow, keeps texture). The ARM outline is specific to this photo.
3. `variants.py arm.jpg unedited.jpg out/` renders the numbered edits shown at weddedblyss.com/photo.

Upload the chosen one with `rsvp photo <file> --home` on the server.
