#!/usr/bin/env bash
#
# Releste repository hygiene checks.
#
# Called by the pre-commit hook; the file names arrive as arguments
# (pass_filenames = true).
#
# Why a hand-written script instead of an off-the-shelf pre-commit-hooks
# package: nixpkgs does not package it, these checks are simple enough to
# express in shell, and keeping them here puts the "must never be committed"
# rules (references/, assets/, shared libraries, .map) in one place.
#
# Usage:
#   releste-lint-files <mode> [--maxkb=N] <file>...
#
# mode:
#   restricted     reject third-party / derived / binary content
#   large          reject files over --maxkb (default 512)
#   whitespace     reject trailing whitespace
#   eof            require a trailing newline
#   line-ending    reject CRLF
#   conflict       reject merge conflict markers
#   private-key    reject private keys
#   case-conflict  reject file names differing only in case
#   ascii          reject non-ASCII bytes (AGENTS.md 5.4)

set -euo pipefail

mode="${1:-}"
shift || true
if [ -z "$mode" ]; then
  echo "usage: releste-lint-files <mode> [files...]" >&2
  exit 2
fi

maxkb=512
if [ "${1:-}" != "" ] && [ "${1#--maxkb=}" != "$1" ]; then
  maxkb="${1#--maxkb=}"
  shift
fi

failed=0
note() { printf '%s\n' "$*" >&2; failed=1; }

# Return the reason a path is restricted (an empty string means allowed).
#
# More specific rules must come before the generic extension rules, otherwise
# they can never match.
restricted_reason() {
  case "$1" in
    libfmod* | fmod*.so | fmod*.dll | fmod*.dylib)
      echo "proprietary FMOD library (the user provides it; point RELESTE_FMOD_LIB at it)"
      ;;
    references/* | assets/* | assets-src/*)
      echo "third-party or derived content (not redistributable)"
      ;;
    maps/*.map)
      echo "engine-native binary map (produced by content-pipeline or the editor)"
      ;;
    *.so | *.dylib | *.dll | *.exe | *.rlib | *.rmeta | *.a | *.o | *.wasm)
      echo "binary file"
      ;;
    *.zip | *.tar | *.tar.gz | *.tar.zst | *.7z)
      echo "archive"
      ;;
    *)
      echo ""
      ;;
  esac
}

# Run the grep/awk style checks on text files only.
is_text() {
  case "$(file -b --mime "$1" 2>/dev/null || echo '')" in
    text/* | *charset=us-ascii* | *charset=utf-8*) return 0 ;;
    *) return 1 ;;
  esac
}

for f in "$@"; do
  [ -e "$f" ] || continue

  case "$mode" in
    restricted)
      reason="$(restricted_reason "$f")"
      if [ -n "$reason" ]; then
        note "FAIL restricted content:$f"
        note "  reason: $reason"
        note "  see .gitignore and references/README.md."
      fi
      ;;

    large)
      [ -f "$f" ] || continue
      size_kb=$(($(wc -c <"$f") / 1024))
      if [ "$size_kb" -gt "$maxkb" ]; then
        note "FAIL file too large (${size_kb} KiB > ${maxkb} KiB):$f"
        note "  if it really belongs in the repository, raise the hook's --maxkb explicitly."
      fi
      ;;

    whitespace)
      is_text "$f" || continue
      if grep -nE '[[:space:]]+$' -- "$f" >/dev/null 2>&1; then
        note "FAIL trailing whitespace:$f"
        grep -nE '[[:space:]]+$' -- "$f" | head -5 | sed 's/^/    /' >&2
      fi
      ;;

    eof)
      is_text "$f" || continue
      [ -s "$f" ] || continue
      if [ -n "$(tail -c 1 -- "$f")" ]; then
        note "FAIL file does not end with a newline:$f"
      fi
      ;;

    line-ending)
      is_text "$f" || continue
      if grep -qU $'\r' -- "$f" 2>/dev/null; then
        note "FAIL contains CRLF:$f"
      fi
      ;;

    conflict)
      is_text "$f" || continue
      if grep -nE '^(<{7}|={7}|>{7})( |$)' -- "$f" >/dev/null 2>&1; then
        note "FAIL merge conflict marker:$f"
        grep -nE '^(<{7}|={7}|>{7})( |$)' -- "$f" | head -5 | sed 's/^/    /' >&2
      fi
      ;;

    private-key)
      is_text "$f" || continue
      if grep -nE 'BEGIN (RSA|DSA|EC|OPENSSH|PGP) PRIVATE KEY' -- "$f" >/dev/null 2>&1; then
        note "FAIL possible private key:$f"
      fi
      ;;

    ascii)
      # Localisation data is the only exception (AGENTS.md 5.4); there is none
      # in the source tree today, so every path is checked.
      case "$f" in
        *l10n* | *i18n* | *locale* | *.po | *.ftl) ;;
        *)
          if [ -n "$(LC_ALL=C tr -d '\0-\177' <"$f" | head -c 1)" ]; then
            note "FAIL non-ASCII byte:$f"
            LC_ALL=C grep -nP '[^\x00-\x7F]' -- "$f" 2>/dev/null | head -5 | sed 's/^/    /' >&2 || true
          fi
          ;;
      esac
      ;;

    case-conflict)
      # Collect the duplicates first, then loop in the **current shell**.
      # `... | while read` would run the loop in a subshell and lose the
      # `failed` flag.
      dups="$(printf '%s\n' "$@" | tr '[:upper:]' '[:lower:]' | sort | uniq -d)"
      if [ -n "$dups" ]; then
        while IFS= read -r dup; do
          [ -n "$dup" ] || continue
          note "FAIL file names differ only in case:$dup"
        done <<<"$dups"
      fi
      ;;

    *)
      echo "unknown mode: $mode" >&2
      exit 2
      ;;
  esac
done

if [ "$failed" -ne 0 ]; then
  exit 1
fi
exit 0
