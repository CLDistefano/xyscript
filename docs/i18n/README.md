# XyScript UI localization

The **code is the source of truth.** Every user-visible label is written as
`tr("some.key", "English")` (menus/dialogs, in `xyscript-gui/src/main.rs`) or
`tr_msg`/`tr_fmt` (status/error messages, in `xyscript-app/src/host.rs`). One
binary serves every language — catalogs load at runtime via the `LANG` command.

## Files

| File | Role |
|------|------|
| `en.lang` | **Generated** English reference (`key = English`). Never edit by hand. |
| `xx.lang` | A language catalog (`fr`, `de`, `es`, `it`, `nl`, `fi`, …). **Edit these to translate.** |
| `pseudo.lang` | **Generated** layout smoke-test locale (accented + padded + bracketed). |
| `i18n-sync.py` | The only tool that writes the `.lang` files. |

Each entry is `key = translation   # English`. The trailing `# English` is the
source string (the fuzzy anchor); a blank value falls back to English. Keep every
`{placeholder}` intact. Lines starting with `#` are comments; `#, fuzzy` above an
entry means its English changed and the translation needs review.

## Load a catalog

```
LANG fi                          # shorthand for fi.lang in the current dir
LANG docs\i18n\fr.lang           # or an explicit path
LANG off                         # back to built-in English
```

## When you add or change a UI string (developers)

1. Add/edit the `tr(...)` call in the code (English ships immediately via fallback).
2. Run the sync:

   ```
   python docs/i18n/i18n-sync.py
   ```

   It rewrites `en.lang`, merges every `xx.lang` (adding new keys blank, flagging
   changed English `#, fuzzy`), and regenerates `pseudo.lang`. The report lists
   untranslated / fuzzy counts per language.
3. `python docs/i18n/i18n-sync.py --check` fails (nonzero) if the checked-in files
   are out of date or have unreviewed fuzzies — wire it into CI so drift can't land.

## Translate (translators)

- Edit the values in an existing `xx.lang`, or seed a new language:

  ```
  python docs/i18n/i18n-sync.py --new sv "SVENSKA" "Swedish"
  ```

- After a source-English change, find `#, fuzzy` markers, fix the translation, and
  delete the marker. Re-running the sync then clears the flag.
