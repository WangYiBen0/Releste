# Translating the Releste editor

This guide is for translators and for anyone adding a language. You do **not**
need Rust, Cargo, or a working build to translate the editor. If you can edit a
text file and open a pull request, you can do this.

If you want the design rationale instead - why the catalogs are runtime data,
how fallback works, why we are not using Fluent or gettext - read
[docs/i18n.md](docs/i18n.md). You do not need it to translate.

---

## 1. The five-minute path: add a new locale

The whole job is one directory and one file.

**1. Pick your locale tag.** It is `<language>_<REGION>`, lowercase language,
uppercase region - `ja_JP`, `pt_BR`, `de_DE`, `ko_KR`. Use the tag you want
to see in the editor's language list.

**2. Copy the English catalog.**

```bash
cp -r locales/en_US locales/ja_JP
```

**3. Translate the values. Never translate the keys.** In
`locales/ja_JP/editor.toml`:

```toml
# before
menu.file.save = "Save"

# after
menu.file.save = "..."
```

Leave everything to the left of the `=` exactly as it is. The key is a stable
identifier that the UI code looks up; changing it orphans the string.

**4. Check your key count still matches.** Keys must not be added, removed, or
renamed - only values change:

```bash
diff <(grep -oE '^"[a-z0-9_.]+"' locales/en_US/editor.toml | sort) \
     <(grep -oE '^"[a-z0-9_.]+"' locales/ja_JP/editor.toml | sort) && echo "key sets match"
```

**5. Open a pull request.** In the description, say:

- the locale tag you added,
- whether you are a native speaker of it,
- anything you deliberately left in English (proper nouns, format names),
- any term you were unsure about.

That is the entire process. No build, no Rust, no code review from an engineer
is required for a pure translation PR - only the native-speaker review in
section 7.

### A partial translation is fine

You do not have to finish. If you translate 40% of the keys and leave the rest
in English, the editor renders your 40% and falls back to English for the rest.
It will not break, and it is a useful contribution. Ship it.

---

## 2. Where the catalogs live, and why

```text
locales/
|-- en_US/
|   `-- editor.toml     <- the source of truth: every key, English strings
`-- zh_CN/
    `-- editor.toml     <- Simplified Chinese
```

- `en_US` is the **reference**. Every key that exists in the code lives there.
  When you are unsure whether a key is real, check `en_US`.
- One directory per locale; the directory name is the locale tag.
- The file is always `editor.toml` inside the directory.

**These files are loaded from disk at run time and are never compiled into the
binary.** Nothing is baked in with `include_str!` and there is no build step
for translations. You can edit `editor.toml` and restart the editor to see your
change; you can also ship an updated catalog next to an already-built editor.
This is why translation never requires a Rust toolchain, and it is the reason
the ASCII rule does not block Chinese, Japanese, or Korean.

---

## 3. Previewing your translation locally

```bash
cargo run -p releste-editor -- --lang ja_JP
```

`--lang` takes the locale tag, i.e. the directory name. If you have a map to
open, pass it first:

```bash
cargo run -p releste-editor -- mymap.map --lang ja_JP
```

Notes:

- An unknown tag is not fatal. The editor warns in the log and shows English.
- To see the log warnings while you work:

  ```bash
  RUST_LOG=releste_editor=debug cargo run -p releste-editor -- --lang ja_JP
  ```

  The loader logs a warning naming any key it could not resolve. This is the
  fastest way to find a key you misspelled.
- If you cannot build the editor at all, the key-set `diff` in section 1 step 4
  and the review in section 7 catch nearly everything without running anything.

---

## 4. Key names: how to read them, and how to find a missing one

Keys are `namespace.area.item`, lowercase, dot-separated:

```toml
app.title              = "Releste Editor"
menu.file.save         = "Save"
panel.rooms            = "Rooms"
inspector.properties   = "Properties"
widget.search          = "Search"
status.no_path         = "pass a path on the command line"
```

They name **where the string appears**, so you can usually guess what you are
translating from the key alone, without looking at code.

**Found an English string in the UI that has no key?** Order of operations:

1. Search `en_US/editor.toml` for the English text, then for a plausible key.
2. If nothing matches, it is either a key that does not exist yet, or a string
   that is still hard-coded in the Rust source (a genuine gap).
3. Open an issue (or mention it in your PR) titled
   `i18n: untranslated string "<the English text>" in <panel>`. Do not invent a
   key name yourself: keys must be added to `en_US/editor.toml` and to the code
   that calls them in the same change, or the two drift.

**Found a key in `en_US` that no longer appears in the UI?** It may be unused.
Say so in an issue rather than deleting it in a translation PR - deleting keys
is a code-side change.

**Never rename a key.** Keys are referenced by code. A rename has to happen in
code, in `en_US`, and in every catalog at once, and it is not a translator's
change to make.

---

## 5. Placeholders

Some strings contain a value substituted at run time:

```toml
status.rooms_loaded = "loaded {count} rooms"
```

**Move the placeholder, do not translate it.** Keep the name inside the braces
exactly as written - `{count}` stays `{count}` - and place it wherever it reads
naturally in your language:

```toml
```

Do not write `{数量}` or rename `{count}` to `{n}`. The name is matched against
what the code supplies.

**What happens if a placeholder is missing from your string?** Nothing breaks.
Substitution only replaces tokens that are present; if your translation has no
`{count}`, the string simply renders without the number. So you may legitimately
drop a placeholder when the count reads badly in your language - but if you are
dropping it because the sentence is awkward, prefer mentioning it in your PR so
the English can be reconsidered.

**What happens if the name does not match?** The token is left in the output
literally, e.g. `{n}`. It shows up on screen and is caught by eye; the loader
does not error on it. This is the main thing to look for when reviewing.

---

## 6. The terminology glossary (canonical)

These terms have **one** agreed translation. Use them everywhere; do not
improvise synonyms, because inconsistent terminology is worse than slightly
imperfect terminology.

### 6.1 Simplified Chinese (`zh_CN`)

The register target is **a mappe`r for Celeste who has used the original
game's level tooling**: terse, concrete, verb-first for actions, no marketing
tone, no formal/legal register, and no glossary-style literal glosses.

| English | Chinese | Notes |
|---|---|---|
| room | 房间 | the editor's core unit; never 屋子 or 空间 |
| tile | 图块 | one cell of a tileset |
| brush | 画笔 | the painting tool, and what you paint with |
| entity | 实体 | a placed interactive object |
| atlas | 图集 | a packed sheet of sprites; not 地图集 |
| cutscene | 过场 | short form of 过场动画; use 过场 in labels, 过场动画 when it stands alone |
| undo | 撤销 | keep 撤销 exactly, including the menu accelerator/tooltip pairing with redo |
| redo | 重做 | paired with 撤销 |
| inspector | 检查器 | the entity property panel; not 检视器 |
| decal | 贴花 | decorative, non-colliding graphics |

Supporting terms that must stay consistent with the above:

| English | Chinese | Notes |
|---|---|---|
| trigger | 触发器 | |
| layer | 图层 | background / foreground are 背景层 / 前景层 |
| tileset | 图块集 | refers to the resource, not the cell (see tile) |
| area | 区域 | a group of rooms |
| bounds | 边界 | |
| sprite | 精灵 | |
| save | 保存 | |
| open | 打开 | |
| cancel | 取消 | |
| delete | 删除 | |
| rename | 重命名 | |
| properties | 属性 | |
| kind | 类型 | the entity kind label (`inspector.kind`); not 种类 or 类别 |
| selection | 选择 | |
| zoom | 缩放 | |
| pan | 平移 | |
| grid | 网格 | |
| snap | 吸附 | |
| level designer / mapper | 关卡设计师 | only if a string needs a person noun |

Rules that apply to the whole table:

- Where the upstream Celeste community already uses a term, match it. A mapper
  should recognise the vocabulary without re-learning it.
- Terse UI verbs: prefer 保存 over 保存文件 when both fit; the extra word is
  noise in a menu.
- Do not use 您. Modern game tooling addresses the user as 你 or drops the
  pronoun entirely with imperative phrasing.
- Punctuation: use full-width Chinese punctuation in Chinese sentences, but keep
  ASCII for placeholders, code, file names, and Latin terms.

### 6.2 For other languages

No glossary exists yet for languages other than `zh_CN`. If you are the first
translator for your language, **add a glossary table for it to this file in the
same PR**. The English column above is the list of concepts that need a decision.
Check whether the original Celeste community in your language already has an
established vocabulary and reuse it.

---

## 7. Review rule

**A new or substantially changed non-English catalog must be reviewed by a
native speaker of that language before it is merged.** No exceptions:

- The author may be the reviewer only if they are a native speaker.
- A reviewer who does not speak the language should not approve on the grounds
  that the keys line up - key-set correctness is necessary, not sufficient.
- Reviewers check, in this order:
  1. natural phrasing (would a mapper say it this way?),
  2. glossary consistency (section 6),
  3. placeholders intact and correctly named (section 5),
  4. register (terse tooling voice, not formal prose),
  5. no leftover English by accident (deliberate English is fine, just say so
     in the PR).
- If no native speaker is available, the PR stays open and is labelled
  `needs-native-review` rather than merged silently.

The reason for the rule is concrete: placeholders the reviewer cannot see and
glosses that read fine to a non-speaker are exactly the defects that reach
users, and users cannot fix them.

---

## 8. The ASCII rule, explained honestly

`AGENTS.md` section 5.4 requires **every tracked file in the repository to be
ASCII-only**, with a single stated exception: *"Localisation data is the only
exception, and it does not live in the source tree."*

A Simplified Chinese catalog is real UTF-8 and is **not** ASCII. This is a
genuine, by-design conflict, and here is exactly how it is resolved:

- The repository's ASCII gate is the `ascii` mode of
  `nix/scripts/releste-lint-files.sh`, installed as the `releste-ascii`
  pre-commit hook by `nix/hooks.nix` (and therefore also covered by
  `nix flake check` and `.pre-commit-config.yaml`).
- That gate already has a **built-in localisation exception**. It skips any path
  matching `*l10n*`, `*i18n*`, `*locale*`, `*.po`, or `*.ftl`.
- Because the catalogs live under `locales/`, their paths match `*locale*` and
  the gate skips them automatically.

**So: no change to `nix/`, `.github/`, or `.pre-commit-config.yaml` is needed,
and none should be made. The exemption already exists and `locales/` is exactly
the right directory name.**

Two things to be careful about:

1. **Do not rename `locales/`.** The exemption is a path glob. A directory named
   `translations/` would not match, the ASCII gate would start checking the
   Chinese catalog, and `nix flake check` would fail. `locales/` matches; leave
   it alone.
2. **The exemption covers the catalogs only, not the rest of your work.** Your
   PR description, your commit message, and any code or documentation you touch
   must stay ASCII. In particular do not paste Chinese into a commit message or
   into `docs/` - put the characters in the catalog file, where they belong.
   English prose in comments, docs, and commits is the project rule
   (`AGENTS.md` section 5.4).

Before you push, check yourself:

```bash
# Should print nothing for English-source files.
grep -nP '[^\x00-\x7F]' locales/en_US/editor.toml
```

`en_US` is ASCII and always should be - if it is not, that is a mistake and the
gate will reject it. A non-English catalog matching this grep is expected and
allowed.

---

## 9. Good first issues

If you are looking for a place to start, these are all real and all
translator-sized:

- [ ] **Finish `zh_CN`.** The Simplified Chinese catalog exists but is
      incomplete; unresolved keys fall back to English. Extend it against
      `locales/en_US/editor.toml` and mention any glossary gaps.
- [ ] **Add `ja_JP`.** Copy `en_US`, translate, add the glossary table for
      Japanese to section 6.2 of this file, and find a native speaker to review.
      This is the highest-value next locale.
- [ ] **Add the locale to the config file.** The language can be set in the
      editor's config (`lang = "zh_CN"`) so that `--lang` is not required every
      run - the config key needs to be documented and defaulted.
- [ ] **Audit for untranslated strings.** Open the editor, walk every panel, and
      file issues for English text that has no key (see section 4). This is
      genuinely useful and needs no translation skill, only patience.
- [ ] **Write a glossary for your language.** Even without a full translation,
      deciding how room / tile / brush / entity / atlas / cutscene / undo /
      redo / inspector / decal are said in your language unblocks whoever
      translates next.
- [ ] **Check the placeholder inventory.** List every `{...}` token in
      `en_US/editor.toml` and confirm each is documented in section 5 with the
      right name. Catches drift early.

---

## 10. Quick reference

```text
add a locale        cp -r locales/en_US locales/<tag>
translate           edit locales/<tag>/editor.toml values only
verify keys         diff <(grep -oE '^"[a-z0-9_.]+"' locales/en_US/editor.toml | sort) \
                         <(grep -oE '^"[a-z0-9_.]+"' locales/<tag>/editor.toml | sort)
preview             cargo run -p releste-editor -- --lang <tag>
debug missing keys  RUST_LOG=releste_editor=debug cargo run -p releste-editor -- --lang <tag>
placeholder         keep {name} exactly; move it, never translate it
ASCII               locales/ is exempt by glob; do not rename the directory
review             a native speaker must approve before merge
```
