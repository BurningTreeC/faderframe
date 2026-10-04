# Reference DDP filesets

These files come from the experiments of the ddp-reverse-eng project
(<https://github.com/ddp-reverse-eng/ddp-reverse-eng>, `exp/`; Copyright (c)
2026 DDP Reverse Engineer, MIT License — see `LICENSE` here). Each folder
holds an experiment's `input.cue` and `run.args` and the DDPID, DDPMS, PQ
descriptor (`SD`), CD-Text and checksum files the DDP Mastering Tools'
cue2ddp wrote for it. `092-ref-cdtext-extended` holds the reference output
for the CD-Text of `091-cdtext-extended` with all six text types (cue2ddp
itself drops COMPOSER, ARRANGER and MESSAGE).

The audio images are not included: the tests regenerate them from the
experiments' position pattern (stereo sample i has left = i & 0xffff,
right = i >> 16) and compare their MD5 with `CHECKSUM.MD5`.
