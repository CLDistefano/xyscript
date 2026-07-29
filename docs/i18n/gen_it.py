#!/usr/bin/env python3
"""Generate a sample ITALIAN catalog from the English template (structure stays in sync)."""
import re, sys
SRC, DST = sys.argv[1], sys.argv[2]

IT = {
    # menu bar
    "menubar.file": "File", "menubar.edit": "Modifica", "menubar.search": "Cerca",
    "menubar.view": "Visualizza", "menubar.document": "Documento", "menubar.tools": "Strumenti",
    "menubar.settings": "Impostazioni", "menubar.help": "Aiuto",
    # File
    "menu.file.new": "Nuovo", "menu.file.open": "Apri…",
    "menu.file.open_readonly": "Apri in sola lettura…", "menu.file.revert": "Ripristina versione salvata",
    "menu.file.save": "Salva", "menu.file.save_as": "Salva con nome…",
    "menu.file.save_selection_as": "Salva selezione con nome…", "menu.file.save_all": "Salva tutto",
    "menu.file.export_pdf": "Esporta come PDF…", "menu.file.close": "Chiudi", "menu.file.quit": "Esci",
    # Edit
    "menu.edit.undo": "Annulla", "menu.edit.redo": "Ripristina", "menu.edit.cut": "Taglia",
    "menu.edit.copy": "Copia", "menu.edit.append_clipboard": "Aggiungi agli appunti",
    "menu.edit.prepend_clipboard": "Anteponi agli appunti", "menu.edit.paste": "Incolla",
    "menu.edit.select_all": "Seleziona tutto", "menu.edit.delete_selection": "Elimina selezione",
    "menu.edit.copy_to_cursor": "Copia al cursore", "menu.edit.move_to_cursor": "Sposta al cursore",
    "menu.edit.case": "Cambia maiuscole/minuscole", "menu.edit.case.upper": "MAIUSCOLO",
    "menu.edit.case.lower": "minuscolo", "menu.edit.case.swap": "Inverti maiuscole/minuscole",
    "menu.edit.text_colour": "Colore del testo…", "menu.edit.clear_text_colour": "Rimuovi colore del testo",
    "menu.edit.insert_file": "Inserisci file…",
    # Search
    "menu.search.find": "Trova…", "menu.search.replace": "Sostituisci…",
    "menu.search.find_next": "Trova successivo", "menu.search.find_prev": "Trova precedente",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Evidenzia",
    "menu.search.emphasize.selection": "Selezione", "menu.search.emphasize.word": "Parola o frase…",
    "menu.search.emphasize.regex": "Espressione regolare…", "menu.search.emphasize.clear": "Cancella",
    # View
    "menu.view.highlighting": "Evidenziazione della sintassi", "menu.view.paragraph_marks": "Segni di paragrafo",
    "menu.view.whitespace": "Spazi vuoti", "menu.view.line_numbers": "Numeri di riga",
    "menu.view.word_wrap": "A capo automatico", "menu.view.wrap_hang": "Rientro sporgente ({n})…",
    "menu.view.preview_pane": "Riquadro di anteprima", "menu.view.shell_pane": "Riquadro shell",
    "menu.view.shell_output": "Scheda output shell", "menu.view.split": "Dividi riquadri",
    "menu.view.split.side_by_side": "Dividi affiancati", "menu.view.split.top_bottom": "Dividi sopra e sotto",
    "menu.view.split.focus_next": "Attiva riquadro successivo", "menu.view.split.swap": "Scambia i due riquadri",
    "menu.view.split.zoom": "Ingrandisci riquadro attivo (attiva/disattiva)", "menu.view.split.fullscreen": "Riquadro attivo a schermo intero",
    "menu.view.split.remove": "Rimuovi divisione", "menu.view.menu_bar": "Barra dei menu",
    "menu.view.title_bar": "Barra del titolo", "menu.view.writers_view": "Modalità scrittura (Esc per uscire)",
    "menu.view.cmdline": "Riga di comando", "menu.view.cmdline.popup": "Finestra popup",
    "menu.view.cmdline.position": "Posizione", "menu.view.cmdline.position.top": "In alto",
    "menu.view.cmdline.position.bottom": "In basso", "menu.view.cmdline.position.hidden": "Nascosta",
    # Document
    "menu.document.doctype": "Tipo di documento", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Automatico (dall'estensione)",
    "menu.document.encoding": "Codifica del testo", "menu.document.encoding.display_as": "Visualizza come",
    "menu.document.encoding.convert_to": "Converti in (nuovo file)", "menu.document.line_endings": "Fine riga",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Sola lettura", "menu.document.word_count": "Conteggio parole",
    "menu.document.char_count": "Conteggio caratteri", "menu.document.properties": "Proprietà…",
    # Tools
    "menu.tools.run_script": "Esegui script…", "menu.tools.run_script_bg": "Esegui script in background…",
    "menu.tools.run_here": "Esegui il codice sotto il cursore", "menu.tools.shell_command": "Comando shell…",
    "menu.tools.jobs": "Attività in background", "menu.tools.lang": "Controllo linguistico",
    "menu.tools.lang.check": "Controlla documento", "menu.tools.lang.spelling": "Sottolinea l'ortografia",
    "menu.tools.lang.grammar": "Sottolinea la grammatica", "menu.tools.lang.spell_next": "Errore di ortografia successivo",
    "menu.tools.lang.spell_prev": "Errore di ortografia precedente", "menu.tools.lang.grammar_next": "Errore grammaticale successivo",
    "menu.tools.lang.grammar_prev": "Errore grammaticale precedente", "menu.tools.lang.fix": "Correggi il problema al cursore…",
    "menu.tools.lang.learn": "Impara le parole selezionate", "menu.tools.lang.dialect": "Variante linguistica",
    "menu.tools.lang.dialect.american": "Americano", "menu.tools.lang.dialect.british": "Britannico",
    "menu.tools.lang.dialect.canadian": "Canadese", "menu.tools.lang.dialect.australian": "Australiano",
    "menu.tools.lang.dialect.indian": "Indiano", "menu.tools.lang.spell_style": "Stile ortografico",
    "menu.tools.lang.grammar_style": "Stile grammaticale", "menu.tools.email": "E-mail",
    "menu.tools.email.list": "Elenca posta in arrivo", "menu.tools.email.save": "Salva messaggi…",
    "menu.tools.email.delete": "Elimina messaggi…", "menu.tools.email.send": "Invia…",
    "menu.tools.email.attach": "Allega file…", "menu.tools.email.clear_attach": "Rimuovi allegati",
    "menu.tools.email.verbose": "Registrazione dettagliata", "menu.tools.library": "Libreria",
    "menu.tools.library.load": "Carica…", "menu.tools.library.reload": "Ricarica",
    "menu.tools.library.unload": "Scarica tutto", "menu.tools.library.list": "Elenca routine",
    "menu.tools.keyboard_chart": "Mappa della tastiera",
    # Settings
    "menu.settings.appearance": "Aspetto", "menu.settings.appearance.colors": "Colori",
    "menu.settings.appearance.colors.editor_text": "Testo dell'editor…", "menu.settings.appearance.colors.editor_bg": "Sfondo dell'editor…",
    "menu.settings.appearance.colors.selection_text": "Testo della selezione…", "menu.settings.appearance.colors.selection_bg": "Sfondo della selezione…",
    "menu.settings.appearance.colors.caret": "Cursore…", "menu.settings.appearance.colors.separators": "Separatori…",
    "menu.settings.appearance.colors.tab_bg": "Sfondo della barra delle schede", "menu.settings.appearance.colors.tab_bg.active": "Scheda attiva…",
    "menu.settings.appearance.colors.tab_bg.hover": "Scheda sotto il puntatore…", "menu.settings.appearance.colors.tab_bg.inactive": "Scheda inattiva…",
    "menu.settings.appearance.colors.tab_close": "Chiudi scheda ×…", "menu.settings.appearance.colors.ui_text": "Testo dell'interfaccia…",
    "menu.settings.appearance.colors.ui_bg": "Sfondo dell'interfaccia…", "menu.settings.appearance.colors.txt_text": "Testo della finestra TXT…",
    "menu.settings.appearance.colors.txt_bg": "Sfondo della finestra TXT…", "menu.settings.appearance.colors.dir_text": "Testo della finestra directory…",
    "menu.settings.appearance.colors.dir_bg": "Sfondo della finestra directory…", "menu.settings.appearance.fonts": "Caratteri",
    "menu.settings.appearance.fonts.editor": "Editor…", "menu.settings.appearance.fonts.ui": "Interfaccia…",
    "menu.settings.appearance.fonts.txt": "Finestra TXT…", "menu.settings.appearance.fonts.dir": "Finestra directory…",
    "menu.settings.appearance.fonts.preview_body": "Corpo del testo anteprima/PDF…", "menu.settings.appearance.fonts.preview_code": "Codice anteprima/PDF…",
    "menu.settings.appearance.ui_zoom": "Zoom dell'interfaccia", "menu.settings.appearance.ui_zoom.in": "Ingrandisci",
    "menu.settings.appearance.ui_zoom.out": "Riduci", "menu.settings.appearance.ui_zoom.reset": "Ripristina (100%)",
    "menu.settings.appearance.margins": "Margini", "menu.settings.appearance.margins.wider": "Più larghi",
    "menu.settings.appearance.margins.narrower": "Più stretti", "menu.settings.appearance.margins.none": "Nessuno",
    "menu.settings.appearance.line_spacing": "Interlinea", "menu.settings.appearance.line_spacing.more": "Maggiore",
    "menu.settings.appearance.line_spacing.less": "Minore", "menu.settings.appearance.reset_ui": "Ripristina dimensione interfaccia",
    "menu.settings.appearance.reset_dw": "Ripristina finestra directory", "menu.settings.syntax": "Evidenziazione della sintassi",
    "menu.settings.syntax.theme_colors": "Colori del tema", "menu.settings.syntax.token_kinds": "Tipi di token…",
    "menu.settings.syntax.reset_theme": "Ripristina tema", "menu.settings.syntax.auto": "Evidenziazione automatica all'apertura",
    "menu.settings.editor": "Comportamento dell'editor", "menu.settings.editor.persistent_sel": "Selezione persistente",
    "menu.settings.editor.remember_geom": "Ricorda dimensione e posizione della finestra", "menu.settings.editor.coding": "Aiuti alla programmazione",
    "menu.settings.editor.coding.autoindent": "Rientra automaticamente le nuove righe", "menu.settings.editor.coding.soft_tabs": "Tabulazioni morbide (inserisci spazi)",
    "menu.settings.editor.coding.hang_indent": "Rientro sporgente per le righe a capo", "menu.settings.editor.coding.match_brackets": "Evidenzia le parentesi corrispondenti",
    "menu.settings.editor.coding.close_brackets": "Chiudi automaticamente parentesi e virgolette", "menu.settings.editor.autosave": "Salvataggio automatico",
    "menu.settings.editor.autosave.off": "Disattivato", "menu.settings.editor.autosave.30s": "Ogni 30 sec.",
    "menu.settings.editor.autosave.1m": "Ogni 1 min.", "menu.settings.editor.autosave.2m": "Ogni 2 min.",
    "menu.settings.editor.autosave.5m": "Ogni 5 min.", "menu.settings.editor.tab_width": "Larghezza tabulazione",
    "menu.settings.editor.precision": "Precisione numerica", "menu.settings.editor.precision.off": "Disattivata (completa)",
    "menu.settings.keyboard": "Tastiera", "menu.settings.keyboard.load": "Carica…",
    "menu.settings.keyboard.save": "Salva…", "menu.settings.keyboard.reset": "Ripristina tastiera integrata",
    "menu.settings.allow_shell": "Consenti comandi shell", "menu.settings.save_settings": "Salva impostazioni…",
    "menu.settings.load_settings": "Carica impostazioni…", "menu.settings.autosave_settings": "Salva automaticamente le impostazioni",
    # Help
    "menu.help.reference": "Riferimento", "menu.help.about": "Informazioni su XyScript",
    # Dialogs (wired)
    "button.close": "Chiudi",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Cerca:",
    "dialog.grep.pattern_hint": "testo (o un'espressione regolare con «Espressione regolare»)",
    "dialog.grep.folder": "Nella cartella:", "dialog.grep.folder_hint": "vuoto = cartella corrente — oppure  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Espressione regolare", "dialog.grep.ignore_case": "Ignora maiuscole/minuscole",
    "dialog.grep.whole_word": "Parola intera", "dialog.grep.search": "Cerca",
    "dialog.run.title": "Esegui comando",
    "dialog.run.shell_off": "I comandi shell sono disattivati — attivali in Impostazioni ⏵ Consenti comandi shell.",
    "dialog.run.command": "Comando:", "dialog.run.command_hint": "un comando shell",
    "dialog.run.background": "Esegui in background", "dialog.run.insert_output": "Inserisci l'output al cursore",
    "dialog.run.run": "Esegui",
    "dialog.find.find_label": "Trova:", "dialog.find.find_hint": "testo da cercare",
    "dialog.find.replace_label": "Sostituisci:", "dialog.find.replace_hint": "testo sostitutivo",
    "dialog.find.case": "Maiuscole/minuscole", "dialog.find.whole_word": "Parola intera",
    "dialog.find.regex": "Espressione regolare", "dialog.find.in_selection": "Nella selezione",
    "dialog.find.keep_case": "Mantieni maiuscole/minuscole nella sostituzione",
    "dialog.find.keep_case_hint": "Sostituisci / Sostituisci tutto: adatta le maiuscole/minuscole di ogni sostituzione al testo sostituito (minuscolo / Iniziale maiuscola / MAIUSCOLO).",
    "dialog.find.find_next": "Successivo (F3)", "dialog.find.find_prev": "Precedente (Maiusc+F3)",
    "dialog.find.replace": "Sostituisci", "dialog.find.replace_all": "Sostituisci tutto",
    "dialog.about.title": "Informazioni su XyScript", "dialog.about.tagline": "Un editor configurabile nella tradizione di XyWrite.",
    # Messages (curated phase-2 batch — keep every {placeholder} intact)
    "msg.search.not_found": "non trovato: {find}",
    "msg.open.not_found": "file non trovato: {path}",
    "msg.save.written": "salvato {name}",
    "msg.readonly": "il documento è in sola lettura (RO per sbloccare)",
    "msg.grep.none": "GREP: nessun risultato",
    "msg.grep.searching": "GREP: ricerca in corso…",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  ITALIANO (sample translation)
# =============================================================================
# A sample Italian catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-it.txt
# Menu labels, dialogs, and the curated status/error messages are all wired
# (phases 1 & 2). Any untranslated key falls back to English. The ~200 XPL
# parser diagnostics stay English by design. {placeholders} kept intact.
# =============================================================================

"""

ENTRY = re.compile(r'^([A-Za-z][\w.]*)\s*=\s*#\s*(.*)$')
out = [HEADER]
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
        val = IT.get(key, "")
        if not val:
            missing.append(key)
        out.append(f"{key} = {val}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}; {len(missing)} untranslated key(s)" + (": " + ", ".join(missing) if missing else ""))
