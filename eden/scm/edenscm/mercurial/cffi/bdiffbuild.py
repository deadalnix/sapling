# Portions Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# Copyright 2016 Mercurial Contributors
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

import os

# pyre-fixme[21]: Could not find `cffi`.
import cffi


ffi = cffi.FFI()
ffi.set_source(
    "mercurial.cffi._bdiff",
    open(os.path.join(os.path.join(os.path.dirname(__file__), ".."), "bdiff.c")).read(),
    include_dirs=["mercurial"],
)
ffi.cdef(
    """
struct bdiff_line {
    int hash, n, e;
    ssize_t len;
    const char *l;
};

struct bdiff_hunk;
struct bdiff_hunk {
    int a1, a2, b1, b2;
    struct bdiff_hunk *next;
};

int bdiff_splitlines(const char *a, ssize_t len, struct bdiff_line **lr);
int bdiff_diff(struct bdiff_line *a, int an, struct bdiff_line *b, int bn,
    struct bdiff_hunk *base);
void bdiff_freehunks(struct bdiff_hunk *l);
void free(void*);
"""
)

if __name__ == "__main__":
    ffi.compile()
