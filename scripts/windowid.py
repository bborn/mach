#!/usr/bin/env python3
"""Print the CGWindowID of a pid's frontmost on-screen window.

This exists so QA can screenshot the app without touching it.

`screencapture -R<rect>` grabs whatever happens to be on that region of the
screen, so it only works if the window is raised — which steals focus from
whoever is using the machine, and an agent iterating visually does that every
few seconds. `screencapture -l<id>` captures the window's own buffer instead:
occluded, backgrounded, behind other apps, it does not matter, and nothing is
raised or focused.

Getting the id needs CoreGraphics. `pyobjc` is not installed and the Swift
toolchain on this machine has a module conflict, so this calls the framework
through `ctypes` — no dependency, no build step.

Usage: windowid.py <pid>   → prints the id, or exits 1 if there is no window.
"""

import ctypes
import ctypes.util
import sys

# Layer 0 is the ordinary window layer; panels, tooltips and menus sit above it.
NORMAL_LAYER = 0
# Windows below this size are transient launch artefacts and capture as blank.
MIN_SIDE = 50


def main() -> int:
    if len(sys.argv) != 2 or not sys.argv[1].isdigit():
        print("usage: windowid.py <pid>", file=sys.stderr)
        return 2
    want_pid = int(sys.argv[1])

    cf_path = ctypes.util.find_library("CoreFoundation")
    cg_path = ctypes.util.find_library("CoreGraphics")
    if not cf_path or not cg_path:
        print("windowid: CoreGraphics unavailable", file=sys.stderr)
        return 2

    cf = ctypes.cdll.LoadLibrary(cf_path)
    cg = ctypes.cdll.LoadLibrary(cg_path)

    # kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    options = (1 << 0) | (1 << 4)
    cg.CGWindowListCopyWindowInfo.restype = ctypes.c_void_p
    cg.CGWindowListCopyWindowInfo.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    array = cg.CGWindowListCopyWindowInfo(options, 0)
    if not array:
        return 1

    cf.CFArrayGetCount.restype = ctypes.c_long
    cf.CFArrayGetCount.argtypes = [ctypes.c_void_p]
    cf.CFArrayGetValueAtIndex.restype = ctypes.c_void_p
    cf.CFArrayGetValueAtIndex.argtypes = [ctypes.c_void_p, ctypes.c_long]
    cf.CFDictionaryGetValue.restype = ctypes.c_void_p
    cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    cf.CFStringCreateWithCString.restype = ctypes.c_void_p
    cf.CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
    cf.CFNumberGetValue.restype = ctypes.c_bool
    cf.CFNumberGetValue.argtypes = [ctypes.c_void_p, ctypes.c_long, ctypes.c_void_p]
    cf.CFRelease.argtypes = [ctypes.c_void_p]

    def key(name: str) -> ctypes.c_void_p:
        # 0x08000100 = kCFStringEncodingUTF8
        return ctypes.c_void_p(cf.CFStringCreateWithCString(None, name.encode(), 0x08000100))

    def number(d: int, name: str):
        k = key(name)
        try:
            ref = cf.CFDictionaryGetValue(ctypes.c_void_p(d), k)
            if not ref:
                return None
            out = ctypes.c_longlong()
            # kCFNumberLongLongType = 11
            if not cf.CFNumberGetValue(ctypes.c_void_p(ref), 11, ctypes.byref(out)):
                return None
            return out.value
        finally:
            cf.CFRelease(k)

    def bounds_side(d: int, side: str):
        k_bounds = key("kCGWindowBounds")
        try:
            ref = cf.CFDictionaryGetValue(ctypes.c_void_p(d), k_bounds)
            if not ref:
                return None
            k_side = key(side)
            try:
                v = cf.CFDictionaryGetValue(ctypes.c_void_p(ref), k_side)
                if not v:
                    return None
                out = ctypes.c_double()
                # kCFNumberDoubleType = 13
                if not cf.CFNumberGetValue(ctypes.c_void_p(v), 13, ctypes.byref(out)):
                    return None
                return out.value
            finally:
                cf.CFRelease(k_side)
        finally:
            cf.CFRelease(k_bounds)

    try:
        # Front-to-back order, so the first match is what a person would call
        # "the window".
        for i in range(cf.CFArrayGetCount(array)):
            d = cf.CFArrayGetValueAtIndex(array, i)
            if number(d, "kCGWindowOwnerPID") != want_pid:
                continue
            if number(d, "kCGWindowLayer") != NORMAL_LAYER:
                continue
            width = bounds_side(d, "Width") or 0
            height = bounds_side(d, "Height") or 0
            if width < MIN_SIDE or height < MIN_SIDE:
                continue
            win = number(d, "kCGWindowNumber")
            if win is None:
                continue
            print(win)
            return 0
    finally:
        cf.CFRelease(ctypes.c_void_p(array))

    return 1


if __name__ == "__main__":
    sys.exit(main())
