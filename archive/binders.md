---
title: binders
tags: 
created: 2026-06-12T02:36:51
---
This document describes two new XPL commands that will use two new sets of special reserved variables to be used primarily to bind text snippets and XPL scripts to keys via the KBD file. One command, `LDTX` ("load text"), will be used to save text to variables named @A-@Z, @0-@1, which when executed will put the assigned text at the insertion point (text or CMline). The other command, `LDPM`, will be used to assign an XPL script to variables named &A-&Z, &0-&9. In the typical usage, these variables would be bound to keys via the KBD file; for example

TABLE=ALT
30=GT;@A

TABLE=CTRL+ALT
30=&A

The combination of the `LDxx` commands and the special reserved variables will enable binding text and XPL to keys without editing the loaded KBD file.

Command usage:
`LDTX x[,<text_file>]`
`LDPM x[,<script_file>]`
If no file specified, input is via selected text
For example:
`LDTX h` loads the selected text to `@H`
`LDTX p,d:\path\myfile.txt` loads the contents of myfile.txt to `@P`
`LDPM b` loads selected XPL code to `&B`
`LDPM k,d:\path\myscript.xss` loads myscript to `&K`

Eventually, the intention is to make the contents of `@` and `&` binder variables persist across sessions via a config file (format to be discussed; probably a JSON-like file that supports comments)
