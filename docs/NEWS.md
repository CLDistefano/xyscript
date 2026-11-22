# XyScript — release notes

What changed in each release, in plain terms. The newest release is first. After an
update, XyScript shows the section for the version you just moved to in a one-time
read-only tab; you can reopen it any time from **Help ▸ What's new**.

Each `## v…` section below is the text that tab displays. When a release is cut,
`packaging/gen-news.py <version>` scaffolds a fresh section from the merged pull
requests since the last tag; trim it to the user-facing highlights before tagging.

## v0.1.17

- **One-click updates from the menu.** When a check finds a newer version, the
  **Help** menu now offers **Install update v…** — it downloads and verifies the
  release in the background and applies it cleanly at the next launch, with no
  commands to type.
- **"New in" release notes.** After an update, XyScript opens a one-time read-only
  tab describing what changed, and **Help ▸ What's new** reopens it on demand.

## v0.1.15

- **Automatic updates.** XyScript can watch its own release channel and tell you
  when a newer version is out (`UPDATEMODE`), download and verify it, and — at your
  choosing — install it at the next launch. Your edited files are never overwritten:
  a changed default is written beside your copy as `<name>.new`, and `UPDATE
  ROLLBACK` reverses an install in one step. See the manual's *Keeping XyScript up
  to date* chapter.
