#!/usr/bin/env bash
#
# Releste 仓库卫生检查。
#
# 由 pre-commit 钩子调用，文件名由参数传入（pass_filenames = true）。
#
# 之所以自己实现而不用现成的 pre-commit-hooks：nixpkgs 没有打包该包，
# 而这几项检查用 shell 表达足够简单，也便于把"不得入库"的规则
# （references/ assets/ 动态库 .map）集中写在一处。
#
# 用法：
#   releste-lint-files <mode> [--maxkb=N] <file>...
#
# mode:
#   restricted     禁止第三方 / 派生 / 二进制内容入库
#   large          单文件超过 --maxkb（默认 512）即拒绝
#   whitespace     禁止行尾空白
#   eof            要求文件以换行结尾
#   line-ending    禁止 CRLF
#   conflict       禁止合并冲突标记
#   private-key    禁止私钥
#   case-conflict  禁止仅大小写不同的文件名

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

# 返回受限原因（空串表示允许）。
#
# 更具体的规则必须放在通用扩展名规则之前，否则永远不会命中。
restricted_reason() {
  case "$1" in
    libfmod* | fmod*.so | fmod*.dll | fmod*.dylib)
      echo "FMOD 专有动态库（请由使用者自备，用 RELESTE_FMOD_LIB 指向）"
      ;;
    references/* | assets/* | assets-src/*)
      echo "第三方资源或由其派生的内容（不得再分发）"
      ;;
    maps/*.map)
      echo "自研二进制地图（由 content-pipeline 或编辑器生成）"
      ;;
    *.so | *.dylib | *.dll | *.exe | *.rlib | *.rmeta | *.a | *.o | *.wasm)
      echo "二进制文件"
      ;;
    *.zip | *.tar | *.tar.gz | *.tar.zst | *.7z)
      echo "归档文件"
      ;;
    *)
      echo ""
      ;;
  esac
}

# 只对文本文件跑 grep/awk 类检查。
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
        note "✗ 禁止提交受限内容：$f"
        note "  原因：$reason"
        note "  见 .gitignore 与 references/README.md。"
      fi
      ;;

    large)
      [ -f "$f" ] || continue
      size_kb=$(($(wc -c <"$f") / 1024))
      if [ "$size_kb" -gt "$maxkb" ]; then
        note "✗ 文件过大（${size_kb} KiB > ${maxkb} KiB）：$f"
        note "  若确需入库，请显式调整钩子的 --maxkb。"
      fi
      ;;

    whitespace)
      is_text "$f" || continue
      if grep -nE '[[:space:]]+$' -- "$f" >/dev/null 2>&1; then
        note "✗ 行尾空白：$f"
        grep -nE '[[:space:]]+$' -- "$f" | head -5 | sed 's/^/    /' >&2
      fi
      ;;

    eof)
      is_text "$f" || continue
      [ -s "$f" ] || continue
      if [ -n "$(tail -c 1 -- "$f")" ]; then
        note "✗ 文件未以换行结尾：$f"
      fi
      ;;

    line-ending)
      is_text "$f" || continue
      if grep -qU $'\r' -- "$f" 2>/dev/null; then
        note "✗ 含 CRLF：$f"
      fi
      ;;

    conflict)
      is_text "$f" || continue
      if grep -nE '^(<{7}|={7}|>{7})( |$)' -- "$f" >/dev/null 2>&1; then
        note "✗ 合并冲突标记：$f"
        grep -nE '^(<{7}|={7}|>{7})( |$)' -- "$f" | head -5 | sed 's/^/    /' >&2
      fi
      ;;

    private-key)
      is_text "$f" || continue
      if grep -nE 'BEGIN (RSA|DSA|EC|OPENSSH|PGP) PRIVATE KEY' -- "$f" >/dev/null 2>&1; then
        note "✗ 疑似私钥：$f"
      fi
      ;;

    case-conflict)
      # 先收集重复项，再在**当前 shell** 里遍历。
      # 直接 `... | while read` 会把 note 放进子 shell，failed 标志会丢。
      dups="$(printf '%s\n' "$@" | tr '[:upper:]' '[:lower:]' | sort | uniq -d)"
      if [ -n "$dups" ]; then
        while IFS= read -r dup; do
          [ -n "$dup" ] || continue
          note "✗ 文件名仅大小写不同：$dup"
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
