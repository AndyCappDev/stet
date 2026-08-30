
## Downloads

`stet-<version>-x86_64-unknown-linux-musl.tar.gz` is the one most people
want: statically linked, no glibc requirement, no runtime libraries, no GUI.
It is the right choice for servers, CI and containers.

The `-gnu` Linux build adds the desktop viewer and needs glibc 2.35 or newer
(Debian 12, Ubuntu 22.04 and later) plus X11 or Wayland and OpenGL to open a
window. Rendering to files works without them.

**macOS**: the binaries are unsigned, so opening a downloaded archive from
Finder trips Gatekeeper ("cannot be opened because the developer cannot be
verified"). Installing with `curl` avoids it entirely, because the quarantine
attribute is set by the downloading application and `curl` does not set one:

```
curl -L <asset-url> | tar xz
```

If you already downloaded it in a browser, clear the flag with
`xattr -dr com.apple.quarantine stet`.

**Windows**: SmartScreen will warn that the publisher is unknown; choose
More info -> Run anyway. Code signing certificates cost money and stet does
not have one.

Rendering is identical across these artifacts with one caveat: the musl
build's libm differs from glibc's in the last bits, which can shift
antialiasing coverage by a pixel or two on heavily curved PostScript
artwork (measured: 265 of 5.4M pixels on one sample, invisible in practice;
all PDF samples tested were byte-identical). Do not generate rendering
baselines with one build and compare them against another.

Verify a download against `SHA256SUMS`.
