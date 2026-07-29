#!/usr/bin/env python3
"""Generate a sample GERMAN catalog from the English template (structure stays in sync)."""
import os, re, sys
SRC, DST = sys.argv[1], sys.argv[2]

DE = {
    # menu bar
    "menubar.file": "Datei", "menubar.edit": "Bearbeiten", "menubar.search": "Suchen",
    "menubar.view": "Ansicht", "menubar.document": "Dokument", "menubar.tools": "Werkzeuge",
    "menubar.settings": "Einstellungen", "menubar.help": "Hilfe",
    # File
    "menu.file.new": "Neu", "menu.file.open": "Öffnen…",
    "menu.file.open_readonly": "Schreibgeschützt öffnen…", "menu.file.revert": "Auf gespeicherte Fassung zurücksetzen",
    "menu.file.save": "Speichern", "menu.file.save_as": "Speichern unter…",
    "menu.file.save_selection_as": "Auswahl speichern unter…", "menu.file.save_all": "Alle speichern",
    "menu.file.export_pdf": "Als PDF exportieren…", "menu.file.close": "Schließen", "menu.file.quit": "Beenden",
    # Edit
    "menu.edit.undo": "Rückgängig", "menu.edit.redo": "Wiederholen", "menu.edit.cut": "Ausschneiden",
    "menu.edit.copy": "Kopieren", "menu.edit.append_clipboard": "An Zwischenablage anhängen",
    "menu.edit.prepend_clipboard": "Vorne an Zwischenablage anfügen", "menu.edit.paste": "Einfügen",
    "menu.edit.select_all": "Alles auswählen", "menu.edit.delete_selection": "Auswahl löschen",
    "menu.edit.copy_to_cursor": "An Cursor kopieren", "menu.edit.move_to_cursor": "An Cursor verschieben",
    "menu.edit.case": "Groß-/Kleinschreibung ändern", "menu.edit.case.upper": "GROSSBUCHSTABEN",
    "menu.edit.case.lower": "kleinbuchstaben", "menu.edit.case.swap": "Groß-/Kleinschreibung umkehren",
    "menu.edit.text_colour": "Textfarbe…", "menu.edit.clear_text_colour": "Textfarbe entfernen",
    "menu.edit.insert_file": "Datei einfügen…",
    # Search
    "menu.search.find": "Suchen…", "menu.search.replace": "Ersetzen…",
    "menu.search.find_next": "Weitersuchen", "menu.search.find_prev": "Rückwärts suchen",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Hervorheben",
    "menu.search.emphasize.selection": "Auswahl", "menu.search.emphasize.word": "Wort oder Wendung…",
    "menu.search.emphasize.regex": "Regulärer Ausdruck…", "menu.search.emphasize.clear": "Löschen",
    # View
    "menu.view.highlighting": "Syntaxhervorhebung", "menu.view.paragraph_marks": "Absatzmarken",
    "menu.view.whitespace": "Leerzeichen", "menu.view.line_numbers": "Zeilennummern",
    "menu.view.word_wrap": "Zeilenumbruch", "menu.view.wrap_hang": "Hängender Einzug ({n})…",
    "menu.view.preview_pane": "Vorschaubereich", "menu.view.shell_pane": "Shell-Bereich",
    "menu.view.shell_output": "Shell-Ausgabe-Tab", "menu.view.split": "Bereiche teilen",
    "menu.view.split.side_by_side": "Nebeneinander teilen", "menu.view.split.top_bottom": "Oben und unten teilen",
    "menu.view.split.focus_next": "Nächsten Bereich aktivieren", "menu.view.split.swap": "Beide Bereiche tauschen",
    "menu.view.split.zoom": "Aktiven Bereich vergrößern (umschalten)", "menu.view.split.fullscreen": "Aktiver Bereich im Vollbild",
    "menu.view.split.remove": "Teilung aufheben", "menu.view.menu_bar": "Menüleiste",
    "menu.view.title_bar": "Titelleiste", "menu.view.writers_view": "Schreibmodus (Esc zum Verlassen)",
    "menu.view.cmdline": "Befehlszeile", "menu.view.cmdline.popup": "Popup-Fenster",
    "menu.view.cmdline.position": "Position", "menu.view.cmdline.position.top": "Oben",
    "menu.view.cmdline.position.bottom": "Unten", "menu.view.cmdline.position.hidden": "Ausgeblendet",
    # Document
    "menu.document.doctype": "Dokumenttyp", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Automatisch (nach Endung)",
    "menu.document.encoding": "Textkodierung", "menu.document.encoding.display_as": "Anzeigen als",
    "menu.document.encoding.convert_to": "Umwandeln in (neue Datei)", "menu.document.line_endings": "Zeilenenden",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Schreibgeschützt", "menu.document.word_count": "Wortanzahl",
    "menu.document.char_count": "Zeichenanzahl", "menu.document.properties": "Eigenschaften…",
    # Tools
    "menu.tools.run_script": "Skript ausführen…", "menu.tools.run_script_bg": "Skript im Hintergrund ausführen…",
    "menu.tools.run_here": "Code unter dem Cursor ausführen", "menu.tools.shell_command": "Shell-Befehl…",
    "menu.tools.jobs": "Hintergrundaufgaben", "menu.tools.lang": "Sprachprüfung",
    "menu.tools.lang.check": "Dokument prüfen", "menu.tools.lang.spelling": "Rechtschreibung unterstreichen",
    "menu.tools.lang.grammar": "Grammatik unterstreichen", "menu.tools.lang.spell_next": "Nächster Rechtschreibfehler",
    "menu.tools.lang.spell_prev": "Vorheriger Rechtschreibfehler", "menu.tools.lang.grammar_next": "Nächster Grammatikfehler",
    "menu.tools.lang.grammar_prev": "Vorheriger Grammatikfehler", "menu.tools.lang.fix": "Problem am Cursor beheben…",
    "menu.tools.lang.learn": "Ausgewählte Wörter lernen", "menu.tools.lang.dialect": "Sprachvariante",
    "menu.tools.lang.dialect.american": "Amerikanisch", "menu.tools.lang.dialect.british": "Britisch",
    "menu.tools.lang.dialect.canadian": "Kanadisch", "menu.tools.lang.dialect.australian": "Australisch",
    "menu.tools.lang.dialect.indian": "Indisch", "menu.tools.lang.spell_style": "Rechtschreibstil",
    "menu.tools.lang.grammar_style": "Grammatikstil", "menu.tools.email": "E-Mail",
    "menu.tools.email.list": "Postfach auflisten", "menu.tools.email.save": "Nachrichten speichern…",
    "menu.tools.email.delete": "Nachrichten löschen…", "menu.tools.email.send": "Senden…",
    "menu.tools.email.attach": "Datei anhängen…", "menu.tools.email.clear_attach": "Anhänge entfernen",
    "menu.tools.email.verbose": "Ausführliche Protokollierung", "menu.tools.library": "Bibliothek",
    "menu.tools.library.load": "Laden…", "menu.tools.library.reload": "Neu laden",
    "menu.tools.library.unload": "Alle entladen", "menu.tools.library.list": "Routinen auflisten",
    "menu.tools.keyboard_chart": "Tastaturbelegung",
    # Settings
    "menu.settings.appearance": "Erscheinungsbild", "menu.settings.appearance.colors": "Farben",
    "menu.settings.appearance.colors.editor_text": "Editortext…", "menu.settings.appearance.colors.editor_bg": "Editorhintergrund…",
    "menu.settings.appearance.colors.selection_text": "Auswahltext…", "menu.settings.appearance.colors.selection_bg": "Auswahlhintergrund…",
    "menu.settings.appearance.colors.caret": "Cursor…", "menu.settings.appearance.colors.separators": "Trennlinien…",
    "menu.settings.appearance.colors.tab_bg": "Hintergrund der Tab-Leiste", "menu.settings.appearance.colors.tab_bg.active": "Aktiver Tab…",
    "menu.settings.appearance.colors.tab_bg.hover": "Tab unter dem Zeiger…", "menu.settings.appearance.colors.tab_bg.inactive": "Inaktiver Tab…",
    "menu.settings.appearance.colors.tab_close": "Tab schließen ×…", "menu.settings.appearance.colors.ui_text": "Oberflächentext…",
    "menu.settings.appearance.colors.ui_bg": "Oberflächenhintergrund…", "menu.settings.appearance.colors.txt_text": "TXT-Fenster-Text…",
    "menu.settings.appearance.colors.txt_bg": "TXT-Fenster-Hintergrund…", "menu.settings.appearance.colors.dir_text": "Verzeichnisfenster-Text…",
    "menu.settings.appearance.colors.dir_bg": "Verzeichnisfenster-Hintergrund…", "menu.settings.appearance.fonts": "Schriftarten",
    "menu.settings.appearance.fonts.editor": "Editor…", "menu.settings.appearance.fonts.ui": "Oberfläche…",
    "menu.settings.appearance.fonts.txt": "TXT-Fenster…", "menu.settings.appearance.fonts.dir": "Verzeichnisfenster…",
    "menu.settings.appearance.fonts.preview_body": "Vorschau-/PDF-Fließtext…", "menu.settings.appearance.fonts.preview_code": "Vorschau-/PDF-Code…",
    "menu.settings.appearance.ui_zoom": "Oberflächen-Zoom", "menu.settings.appearance.ui_zoom.in": "Vergrößern",
    "menu.settings.appearance.ui_zoom.out": "Verkleinern", "menu.settings.appearance.ui_zoom.reset": "Zurücksetzen (100 %)",
    "menu.settings.appearance.margins": "Ränder", "menu.settings.appearance.margins.wider": "Breiter",
    "menu.settings.appearance.margins.narrower": "Schmaler", "menu.settings.appearance.margins.none": "Keine",
    "menu.settings.appearance.line_spacing": "Zeilenabstand", "menu.settings.appearance.line_spacing.more": "Mehr",
    "menu.settings.appearance.line_spacing.less": "Weniger", "menu.settings.appearance.reset_ui": "Oberflächengröße zurücksetzen",
    "menu.settings.appearance.reset_dw": "Verzeichnisfenster zurücksetzen", "menu.settings.syntax": "Syntaxhervorhebung",
    "menu.settings.syntax.theme_colors": "Themenfarben", "menu.settings.syntax.token_kinds": "Token-Arten…",
    "menu.settings.syntax.reset_theme": "Thema zurücksetzen", "menu.settings.syntax.auto": "Automatische Hervorhebung beim Öffnen",
    "menu.settings.editor": "Editorverhalten", "menu.settings.editor.persistent_sel": "Dauerhafte Auswahl",
    "menu.settings.editor.remember_geom": "Fenstergröße und -position merken", "menu.settings.editor.coding": "Programmierhilfen",
    "menu.settings.editor.coding.autoindent": "Neue Zeilen automatisch einrücken", "menu.settings.editor.coding.soft_tabs": "Weiche Tabs (Leerzeichen einfügen)",
    "menu.settings.editor.coding.hang_indent": "Hängender Einzug für umbrochene Zeilen", "menu.settings.editor.coding.match_brackets": "Zusammengehörige Klammern hervorheben",
    "menu.settings.editor.coding.close_brackets": "Klammern und Anführungszeichen automatisch schließen", "menu.settings.editor.autosave": "Automatisches Speichern",
    "menu.settings.editor.autosave.off": "Aus", "menu.settings.editor.autosave.30s": "Alle 30 Sek.",
    "menu.settings.editor.autosave.1m": "Alle 1 Min.", "menu.settings.editor.autosave.2m": "Alle 2 Min.",
    "menu.settings.editor.autosave.5m": "Alle 5 Min.", "menu.settings.editor.tab_width": "Tabulatorbreite",
    "menu.settings.editor.precision": "Numerische Genauigkeit", "menu.settings.editor.precision.off": "Aus (voll)",
    "menu.settings.keyboard": "Tastatur", "menu.settings.keyboard.load": "Laden…",
    "menu.settings.keyboard.save": "Speichern…", "menu.settings.keyboard.reset": "Auf integrierte Tastatur zurücksetzen",
    "menu.settings.allow_shell": "Shell-Befehle zulassen", "menu.settings.save_settings": "Einstellungen speichern…",
    "menu.settings.load_settings": "Einstellungen laden…", "menu.settings.autosave_settings": "Einstellungen automatisch speichern",
    # Help
    "menu.help.reference": "Referenz", "menu.help.about": "Über XyScript",
    # Dialogs (wired)
    "button.close": "Schließen",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Suchen:",
    "dialog.grep.pattern_hint": "Text (oder ein regulärer Ausdruck mit „Regulärer Ausdruck“)",
    "dialog.grep.folder": "Im Ordner:", "dialog.grep.folder_hint": "leer = aktueller Ordner — oder  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Regulärer Ausdruck", "dialog.grep.ignore_case": "Groß-/Kleinschreibung ignorieren",
    "dialog.grep.whole_word": "Ganzes Wort", "dialog.grep.search": "Suchen",
    "dialog.run.title": "Befehl ausführen",
    "dialog.run.shell_off": "Shell-Befehle sind deaktiviert — aktivieren Sie sie unter Einstellungen ⏵ Shell-Befehle zulassen.",
    "dialog.run.command": "Befehl:", "dialog.run.command_hint": "ein Shell-Befehl",
    "dialog.run.background": "Im Hintergrund ausführen", "dialog.run.insert_output": "Ausgabe an Cursor einfügen",
    "dialog.run.run": "Ausführen",
    "dialog.find.find_label": "Suchen:", "dialog.find.find_hint": "zu suchender Text",
    "dialog.find.replace_label": "Ersetzen:", "dialog.find.replace_hint": "Ersatztext",
    "dialog.find.case": "Groß-/Kleinschreibung beachten", "dialog.find.whole_word": "Ganzes Wort",
    "dialog.find.regex": "Regulärer Ausdruck", "dialog.find.in_selection": "In Auswahl",
    "dialog.find.keep_case": "Groß-/Kleinschreibung beim Ersetzen beibehalten",
    "dialog.find.keep_case_hint": "Ersetzen / Alle ersetzen: passt die Groß-/Kleinschreibung jedes Ersatzes an den ersetzten Text an (kleinbuchstaben / Großgeschrieben / GROSSBUCHSTABEN).",
    "dialog.find.find_next": "Weiter (F3)", "dialog.find.find_prev": "Zurück (Umschalt+F3)",
    "dialog.find.replace": "Ersetzen", "dialog.find.replace_all": "Alle ersetzen",
    "dialog.about.title": "Über XyScript", "dialog.about.tagline": "Ein konfigurierbarer Editor in der XyWrite-Tradition.",
    # Messages (curated phase-2 batch — keep every {placeholder} intact)
    "msg.search.not_found": "nicht gefunden: {find}",
    "msg.open.not_found": "Datei nicht gefunden: {path}",
    "msg.save.written": "gespeichert {name}",
    "msg.readonly": "Dokument ist schreibgeschützt (RO zum Entsperren)",
    "msg.grep.none": "GREP: keine Treffer",
    "msg.grep.searching": "GREP: Suche läuft…",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  DEUTSCH (sample translation)
# =============================================================================
# A sample German catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-de.txt
# Menu labels, dialogs, and the curated status/error messages are all wired
# (phases 1 & 2). Any untranslated key falls back to English. The ~200 XPL
# parser diagnostics stay English by design. {placeholders} kept intact.
# =============================================================================

"""

ENTRY = re.compile(r'^([A-Za-z][\w.]*)\s*=\s*#\s*(.*)$')
out = [re.sub(r'xyscript-strings-[a-z]{2}\.\w+', os.path.basename(DST), HEADER)]
started = False
missing = []
for raw in open(SRC, encoding='utf-8'):
    line = raw.rstrip('\n')
    if not started:
        if line.strip().endswith('1. MENUS'):
            out.append('# =============================================================================')
            out.append(line)
            started = True
        continue
    m = ENTRY.match(line)
    if m:
        key, comment = m.group(1), m.group(2)
        val = DE.get(key, "")
        if not val:
            missing.append(key)
        out.append(f"{key} = {val}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}; {len(missing)} untranslated key(s)" + (": " + ", ".join(missing) if missing else ""))
