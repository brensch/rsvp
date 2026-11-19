# Home photo edits

How the save-the-date photo (PXL_20260503_033839546.jpg) was edited. Needs numpy and Pillow.
Run from this folder (the scripts load `colour.py` beside them).

1. `colour.py correct <src> v1.jpg '{"wb_gain":[0.896,1,0.993],"hue_shift_deg":5.5,"chroma_cut":0.04}'`
   crops (CROP), partially white-balances against the napkin, and moves skin hue from ~34-38 deg to
   ~41-46 deg (CIELAB) through a soft skin mask: still warm, not sunburnt. (The stronger first
   version, wb 0.849 / hue 10 / chroma 0.08, reached ~47-52 deg and read slightly pale.)
   `colour.py analyse <src>` prints the measurements.
2. `arm.py v1.jpg arm.jpg 0.85` evens the shadow on Alyssa's upper arm with frequency separation
   (lifts the broad shadow, keeps texture). The ARM outline is specific to this photo.
3. `variants.py arm.jpg unedited.jpg out/photo` renders the numbered edits, and
   `make_page.py out/photo` builds the page; copy the folder to /var/www/photo for weddedblyss.com/photo.

Upload the chosen one with `rsvp photo <file> --home` on the server.
