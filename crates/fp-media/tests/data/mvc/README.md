# MVC (3D Blu-ray) test streams

Two-view H.264 MVC streams for the `mvc` module's tests. The expected
per-view hashes in `src/mvc/tests.rs` are those of
[edge264-mvc](https://github.com/jens-duttke/edge264-mvc)'s conformance
manifest (`tests/conformance/manifest.txt`, at b6ee3c3), which anchors them
to JMVC, the MVC reference decoder (ITU-T H.264.2); for the synthetic
streams, to their by-construction output (every sample 128).

| File | What it covers | Source |
|---|---|---|
| `MVCDS-4.264` | ITU MVC conformance stream (CABAC, inter-view prediction) | ITU-T/JVT MVC conformance bitstreams, freely redistributable test material: https://www.itu.int/wftp3/av-arch/jvt-site/draft_conformance/MVC/ |
| `interview_colocated.mkv` | B slices of the second view whose `RefPicList1[0]` is the inter-view reference (colZeroFlag); muxed like a MakeMKV rip: one access unit per block, `avcC` with an `mvcC` box, StereoMode 13 | `tests/conformance/mvc-jmvc/interview_colocated.264` of edge264-mvc (BSD-3-Clause, encoded with JMVC from FFmpeg's `testsrc2`), muxed with a small script |
| `interview_colocated_shifted.mkv` | The same, as mkvmerge v82 muxes MVC: no `mvcC`, each access unit's second view at the start of the next block | as above |
| `mvc_display_ordering.264` | CAVLC with I_PCM macroblocks, base view decoded out of display order | `tests/conformance/mvc-synthetic/` of edge264-mvc (BSD-3-Clause) |
| `mvc_base.264` | Temporal direct prediction, which the rewrite leaves to 2D | as above |

edge264-mvc is Copyright (c) 2013-2014 Celticom / TVLabs, (c) 2014-2026
Thibault Raffaillac, (c) 2026 Jens Duttke, under the BSD 3-Clause license.
