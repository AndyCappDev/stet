# PostScript Test Corpus

Public PostScript and EPS files for automated visual regression testing of stet against GhostScript.

**This directory is not committed to git.** Run `scripts/fetch-corpus.sh` to populate it.

## Sources

| Directory | Source | License | Description |
|-----------|--------|---------|-------------|
| `arxiv/` | [arXiv](https://arxiv.org/) | Various (author-submitted) | PostScript papers, mostly dvips-generated (1990s-2010s) |
| `eps-clipart/` | [EPS Clipart Library](https://epscliparts.sourceforge.net/) | Public domain | 6,500+ EPS clipart files |
| `fsu/` | [FSU EPS/PS Collections](https://people.sc.fsu.edu/~jburkardt/data/eps/eps.html) | Public domain / academic | Curated academic EPS and PS examples |
| `lancaster/` | [Don Lancaster's Guru's Lair](https://www.tinaja.com/post01.shtml) | Freely available | Hand-written PostScript from a legendary PS programmer |
| `ghostscript/` | [GhostScript](https://github.com/ArtifexSoftware/ghostpdl) | AGPL-3.0 | Example PS files distributed with GhostScript |
| `github/` | Various GitHub repos | Various | Community PostScript examples and test files |
| `ctan/` | [CTAN](https://ctan.org/) | LPPL / various | PSTricks examples, dvips test output, TeX-related PS |
| `public-domain-vectors/` | [Public Domain Vectors](https://publicdomainvectors.org/) | CC0 / Public domain | EPS vector graphics from various generators |

## Usage

```bash
# Download all sources
./scripts/fetch-corpus.sh

# Download specific source(s)
./scripts/fetch-corpus.sh fsu ghostscript

# Re-download a source
./scripts/fetch-corpus.sh --clean fsu fsu

# Limit files per source
./scripts/fetch-corpus.sh --limit 100

# Just regenerate manifest
./scripts/fetch-corpus.sh --manifest

# Run corpus tests
./scripts/test-corpus.sh

# Quick test (random 100 files)
./scripts/test-corpus.sh --quick

# Test specific source
./scripts/test-corpus.sh --source fsu

# Test with existing pixeldiff.sh
./scripts/pixeldiff.sh tests/corpus/fsu/eps/*.eps
```

## Per-File Thresholds

Edit `thresholds.conf` to set custom RMSE thresholds for specific files:

```
# filename                    threshold
some_tricky_file.ps           0.10
known_font_diff.eps           0.15
```

## File Structure

```
tests/corpus/
├── README.md              # This file (committed)
├── thresholds.conf        # Per-file thresholds (committed)
├── manifest.txt           # Auto-generated file counts (not committed)
├── arxiv/                 # arXiv PS files
├── eps-clipart/           # SourceForge EPS clipart
├── fsu/                   # FSU academic examples
├── lancaster/             # Don Lancaster's PS files
├── ghostscript/           # GhostScript examples
├── github/                # GitHub community PS
├── ctan/                  # TeX/dvips/PSTricks output
└── public-domain-vectors/ # publicdomainvectors.org EPS
```
