#!/usr/bin/env python3
"""Generate a sample DUTCH catalog from the English template (structure stays in sync)."""
import os, re, sys
SRC, DST = sys.argv[1], sys.argv[2]

NL = {
    # menu bar
    "menubar.file": "Bestand", "menubar.edit": "Bewerken", "menubar.search": "Zoeken",
    "menubar.view": "Beeld", "menubar.document": "Document", "menubar.tools": "Extra",
    "menubar.settings": "Instellingen", "menubar.help": "Help",
    # File
    "menu.file.new": "Nieuw", "menu.file.open": "Openen…",
    "menu.file.open_readonly": "Alleen-lezen openen…", "menu.file.revert": "Terug naar opgeslagen versie",
    "menu.file.save": "Opslaan", "menu.file.save_as": "Opslaan als…",
    "menu.file.save_selection_as": "Selectie opslaan als…", "menu.file.save_all": "Alles opslaan",
    "menu.file.export_pdf": "Exporteren als PDF…", "menu.file.close": "Sluiten", "menu.file.quit": "Afsluiten",
    # Edit
    "menu.edit.undo": "Ongedaan maken", "menu.edit.redo": "Opnieuw", "menu.edit.cut": "Knippen",
    "menu.edit.copy": "Kopiëren", "menu.edit.append_clipboard": "Toevoegen aan klembord",
    "menu.edit.prepend_clipboard": "Vooraan aan klembord toevoegen", "menu.edit.paste": "Plakken",
    "menu.edit.select_all": "Alles selecteren", "menu.edit.delete_selection": "Selectie verwijderen",
    "menu.edit.copy_to_cursor": "Kopiëren naar cursor", "menu.edit.move_to_cursor": "Verplaatsen naar cursor",
    "menu.edit.case": "Hoofdlettergebruik wijzigen", "menu.edit.case.upper": "HOOFDLETTERS",
    "menu.edit.case.lower": "kleine letters", "menu.edit.case.swap": "Hoofdlettergebruik omkeren",
    "menu.edit.text_colour": "Tekstkleur…", "menu.edit.clear_text_colour": "Tekstkleur verwijderen",
    "menu.edit.insert_file": "Bestand invoegen…",
    # Search
    "menu.search.find": "Zoeken…", "menu.search.replace": "Vervangen…",
    "menu.search.find_next": "Volgende zoeken", "menu.search.find_prev": "Vorige zoeken",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Markeren",
    "menu.search.emphasize.selection": "Selectie", "menu.search.emphasize.word": "Woord of woordgroep…",
    "menu.search.emphasize.regex": "Reguliere expressie…", "menu.search.emphasize.clear": "Wissen",
    # View
    "menu.view.highlighting": "Syntaxismarkering", "menu.view.paragraph_marks": "Alineamarkeringen",
    "menu.view.whitespace": "Witruimte", "menu.view.line_numbers": "Regelnummers",
    "menu.view.word_wrap": "Regelterugloop", "menu.view.wrap_hang": "Verspringende inspringing ({n})…",
    "menu.view.preview_pane": "Voorbeeldvenster", "menu.view.shell_pane": "Shell-venster",
    "menu.view.shell_output": "Shell-uitvoer-tabblad", "menu.view.split": "Vensters splitsen",
    "menu.view.split.side_by_side": "Naast elkaar splitsen", "menu.view.split.top_bottom": "Boven en onder splitsen",
    "menu.view.split.focus_next": "Volgend venster activeren", "menu.view.split.swap": "Beide vensters verwisselen",
    "menu.view.split.zoom": "Actief venster vergroten (schakelen)", "menu.view.split.fullscreen": "Actief venster op volledig scherm",
    "menu.view.split.remove": "Splitsing opheffen", "menu.view.menu_bar": "Menubalk",
    "menu.view.title_bar": "Titelbalk", "menu.view.writers_view": "Schrijfmodus (Esc om te sluiten)",
    "menu.view.cmdline": "Opdrachtregel", "menu.view.cmdline.popup": "Pop-upvenster",
    "menu.view.cmdline.position": "Positie", "menu.view.cmdline.position.top": "Boven",
    "menu.view.cmdline.position.bottom": "Onder", "menu.view.cmdline.position.hidden": "Verborgen",
    # Document
    "menu.document.doctype": "Documenttype", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Automatisch (op extensie)",
    "menu.document.encoding": "Tekstcodering", "menu.document.encoding.display_as": "Weergeven als",
    "menu.document.encoding.convert_to": "Converteren naar (nieuw bestand)", "menu.document.line_endings": "Regeleinden",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Alleen-lezen", "menu.document.word_count": "Aantal woorden",
    "menu.document.char_count": "Aantal tekens", "menu.document.properties": "Eigenschappen…",
    # Tools
    "menu.tools.run_script": "Script uitvoeren…", "menu.tools.run_script_bg": "Script op de achtergrond uitvoeren…",
    "menu.tools.run_here": "Code onder de cursor uitvoeren", "menu.tools.shell_command": "Shell-opdracht…",
    "menu.tools.jobs": "Achtergrondtaken", "menu.tools.lang": "Taalcontrole",
    "menu.tools.lang.check": "Document controleren", "menu.tools.lang.spelling": "Spelling onderstrepen",
    "menu.tools.lang.grammar": "Grammatica onderstrepen", "menu.tools.lang.spell_next": "Volgende spelfout",
    "menu.tools.lang.spell_prev": "Vorige spelfout", "menu.tools.lang.grammar_next": "Volgende grammaticafout",
    "menu.tools.lang.grammar_prev": "Vorige grammaticafout", "menu.tools.lang.fix": "Probleem bij cursor oplossen…",
    "menu.tools.lang.learn": "Geselecteerde woorden leren", "menu.tools.lang.dialect": "Taalvariant",
    "menu.tools.lang.dialect.american": "Amerikaans", "menu.tools.lang.dialect.british": "Brits",
    "menu.tools.lang.dialect.canadian": "Canadees", "menu.tools.lang.dialect.australian": "Australisch",
    "menu.tools.lang.dialect.indian": "Indiaas", "menu.tools.lang.spell_style": "Spellingstijl",
    "menu.tools.lang.grammar_style": "Grammaticastijl", "menu.tools.email": "E-mail",
    "menu.tools.email.list": "Postvak weergeven", "menu.tools.email.save": "Berichten opslaan…",
    "menu.tools.email.delete": "Berichten verwijderen…", "menu.tools.email.send": "Verzenden…",
    "menu.tools.email.attach": "Bestand bijvoegen…", "menu.tools.email.clear_attach": "Bijlagen verwijderen",
    "menu.tools.email.verbose": "Uitgebreide logboekregistratie", "menu.tools.library": "Bibliotheek",
    "menu.tools.library.load": "Laden…", "menu.tools.library.reload": "Opnieuw laden",
    "menu.tools.library.unload": "Alles ontladen", "menu.tools.library.list": "Routines weergeven",
    "menu.tools.keyboard_chart": "Toetsenbordindeling",
    # Settings
    "menu.settings.appearance": "Weergave", "menu.settings.appearance.colors": "Kleuren",
    "menu.settings.appearance.colors.editor_text": "Editortekst…", "menu.settings.appearance.colors.editor_bg": "Editorachtergrond…",
    "menu.settings.appearance.colors.selection_text": "Selectietekst…", "menu.settings.appearance.colors.selection_bg": "Selectieachtergrond…",
    "menu.settings.appearance.colors.caret": "Cursor…", "menu.settings.appearance.colors.separators": "Scheidingslijnen…",
    "menu.settings.appearance.colors.tab_bg": "Achtergrond van tabbalk", "menu.settings.appearance.colors.tab_bg.active": "Actief tabblad…",
    "menu.settings.appearance.colors.tab_bg.hover": "Tabblad onder de aanwijzer…", "menu.settings.appearance.colors.tab_bg.inactive": "Inactief tabblad…",
    "menu.settings.appearance.colors.tab_close": "Tabblad sluiten ×…", "menu.settings.appearance.colors.ui_text": "Interfacetekst…",
    "menu.settings.appearance.colors.ui_bg": "Interfaceachtergrond…", "menu.settings.appearance.colors.txt_text": "TXT-venstertekst…",
    "menu.settings.appearance.colors.txt_bg": "TXT-vensterachtergrond…", "menu.settings.appearance.colors.dir_text": "Mapvenstertekst…",
    "menu.settings.appearance.colors.dir_bg": "Mapvensterachtergrond…", "menu.settings.appearance.fonts": "Lettertypen",
    "menu.settings.appearance.fonts.editor": "Editor…", "menu.settings.appearance.fonts.ui": "Interface…",
    "menu.settings.appearance.fonts.txt": "TXT-venster…", "menu.settings.appearance.fonts.dir": "Mapvenster…",
    "menu.settings.appearance.fonts.preview_body": "Voorbeeld-/PDF-bodytekst…", "menu.settings.appearance.fonts.preview_code": "Voorbeeld-/PDF-code…",
    "menu.settings.appearance.ui_zoom": "Interface-zoom", "menu.settings.appearance.ui_zoom.in": "Vergroten",
    "menu.settings.appearance.ui_zoom.out": "Verkleinen", "menu.settings.appearance.ui_zoom.reset": "Herstellen (100 %)",
    "menu.settings.appearance.margins": "Marges", "menu.settings.appearance.margins.wider": "Breder",
    "menu.settings.appearance.margins.narrower": "Smaller", "menu.settings.appearance.margins.none": "Geen",
    "menu.settings.appearance.line_spacing": "Regelafstand", "menu.settings.appearance.line_spacing.more": "Meer",
    "menu.settings.appearance.line_spacing.less": "Minder", "menu.settings.appearance.reset_ui": "Interfacegrootte herstellen",
    "menu.settings.appearance.reset_dw": "Mapvenster herstellen", "menu.settings.syntax": "Syntaxismarkering",
    "menu.settings.syntax.theme_colors": "Themakleuren", "menu.settings.syntax.token_kinds": "Token-soorten…",
    "menu.settings.syntax.reset_theme": "Thema herstellen", "menu.settings.syntax.auto": "Automatisch markeren bij openen",
    "menu.settings.editor": "Editorgedrag", "menu.settings.editor.persistent_sel": "Blijvende selectie",
    "menu.settings.editor.remember_geom": "Venstergrootte en -positie onthouden", "menu.settings.editor.coding": "Programmeerhulp",
    "menu.settings.editor.coding.autoindent": "Nieuwe regels automatisch inspringen", "menu.settings.editor.coding.soft_tabs": "Zachte tabs (spaties invoegen)",
    "menu.settings.editor.coding.hang_indent": "Verspringende inspringing voor teruglopende regels", "menu.settings.editor.coding.match_brackets": "Bijbehorende haakjes markeren",
    "menu.settings.editor.coding.close_brackets": "Haakjes en aanhalingstekens automatisch sluiten", "menu.settings.editor.autosave": "Automatisch opslaan",
    "menu.settings.editor.autosave.off": "Uit", "menu.settings.editor.autosave.30s": "Elke 30 sec.",
    "menu.settings.editor.autosave.1m": "Elke 1 min.", "menu.settings.editor.autosave.2m": "Elke 2 min.",
    "menu.settings.editor.autosave.5m": "Elke 5 min.", "menu.settings.editor.tab_width": "Tabbreedte",
    "menu.settings.editor.precision": "Numerieke precisie", "menu.settings.editor.precision.off": "Uit (volledig)",
    "menu.settings.keyboard": "Toetsenbord", "menu.settings.keyboard.load": "Laden…",
    "menu.settings.keyboard.save": "Opslaan…", "menu.settings.keyboard.reset": "Terugzetten naar ingebouwd toetsenbord",
    "menu.settings.allow_shell": "Shell-opdrachten toestaan", "menu.settings.save_settings": "Instellingen opslaan…",
    "menu.settings.load_settings": "Instellingen laden…", "menu.settings.autosave_settings": "Instellingen automatisch opslaan",
    # Help
    "menu.help.reference": "Naslagwerk", "menu.help.about": "Over XyScript",
    # Dialogs (wired)
    "button.close": "Sluiten",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Zoeken:",
    "dialog.grep.pattern_hint": "Tekst (of een reguliere expressie met „Reguliere expressie”)",
    "dialog.grep.folder": "In map:", "dialog.grep.folder_hint": "leeg = huidige map — of  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Reguliere expressie", "dialog.grep.ignore_case": "Hoofdletters negeren",
    "dialog.grep.whole_word": "Heel woord", "dialog.grep.search": "Zoeken",
    "dialog.run.title": "Opdracht uitvoeren",
    "dialog.run.shell_off": "Shell-opdrachten zijn uitgeschakeld — schakel ze in via Instellingen ⏵ Shell-opdrachten toestaan.",
    "dialog.run.command": "Opdracht:", "dialog.run.command_hint": "een shell-opdracht",
    "dialog.run.background": "Op de achtergrond uitvoeren", "dialog.run.insert_output": "Uitvoer bij cursor invoegen",
    "dialog.run.run": "Uitvoeren",
    "dialog.find.find_label": "Zoeken:", "dialog.find.find_hint": "te zoeken tekst",
    "dialog.find.replace_label": "Vervangen:", "dialog.find.replace_hint": "vervangende tekst",
    "dialog.find.case": "Hoofdlettergevoelig", "dialog.find.whole_word": "Heel woord",
    "dialog.find.regex": "Reguliere expressie", "dialog.find.in_selection": "In selectie",
    "dialog.find.keep_case": "Hoofdlettergebruik behouden bij vervangen",
    "dialog.find.keep_case_hint": "Vervangen / Alles vervangen: past het hoofdlettergebruik van elke vervanging aan aan de vervangen tekst (kleine letters / Beginhoofdletter / HOOFDLETTERS).",
    "dialog.find.find_next": "Volgende (F3)", "dialog.find.find_prev": "Vorige (Shift+F3)",
    "dialog.find.replace": "Vervangen", "dialog.find.replace_all": "Alles vervangen",
    "dialog.about.title": "Over XyScript", "dialog.about.tagline": "Een configureerbare editor in de XyWrite-traditie.",
    # Messages (curated phase-2 batch — keep every {placeholder} intact)
    "msg.search.not_found": "niet gevonden: {find}",
    "msg.open.not_found": "bestand niet gevonden: {path}",
    "msg.save.written": "opgeslagen {name}",
    "msg.readonly": "document is alleen-lezen (RO om te ontgrendelen)",
    "msg.grep.none": "GREP: geen treffers",
    "msg.grep.searching": "GREP: bezig met zoeken…",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  NEDERLANDS (sample translation)
# =============================================================================
# A sample Dutch catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-nl.txt
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
        val = NL.get(key, "")
        if not val:
            missing.append(key)
        out.append(f"{key} = {val}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}; {len(missing)} untranslated key(s)" + (": " + ", ".join(missing) if missing else ""))
