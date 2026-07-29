#!/usr/bin/env python3
"""Generate a sample FINNISH catalog from the English template (structure stays in sync)."""
import os, re, sys
SRC, DST = sys.argv[1], sys.argv[2]

FI = {
    # menu bar
    "menubar.file": "Tiedosto", "menubar.edit": "Muokkaa", "menubar.search": "Etsi",
    "menubar.view": "Näytä", "menubar.document": "Asiakirja", "menubar.tools": "Työkalut",
    "menubar.settings": "Asetukset", "menubar.help": "Ohje",
    # File
    "menu.file.new": "Uusi", "menu.file.open": "Avaa…",
    "menu.file.open_readonly": "Avaa vain luettavaksi…", "menu.file.revert": "Palauta tallennettu versio",
    "menu.file.save": "Tallenna", "menu.file.save_as": "Tallenna nimellä…",
    "menu.file.save_selection_as": "Tallenna valinta nimellä…", "menu.file.save_all": "Tallenna kaikki",
    "menu.file.export_pdf": "Vie PDF-muotoon…", "menu.file.close": "Sulje", "menu.file.quit": "Lopeta",
    # Edit
    "menu.edit.undo": "Kumoa", "menu.edit.redo": "Tee uudelleen", "menu.edit.cut": "Leikkaa",
    "menu.edit.copy": "Kopioi", "menu.edit.append_clipboard": "Lisää leikepöydän loppuun",
    "menu.edit.prepend_clipboard": "Lisää leikepöydän alkuun", "menu.edit.paste": "Liitä",
    "menu.edit.select_all": "Valitse kaikki", "menu.edit.delete_selection": "Poista valinta",
    "menu.edit.copy_to_cursor": "Kopioi kohdistimeen", "menu.edit.move_to_cursor": "Siirrä kohdistimeen",
    "menu.edit.case": "Muuta kirjainkokoa", "menu.edit.case.upper": "SUURAAKKOSET",
    "menu.edit.case.lower": "pienaakkoset", "menu.edit.case.swap": "Vaihda kirjainkoko",
    "menu.edit.text_colour": "Tekstin väri…", "menu.edit.clear_text_colour": "Poista tekstin väri",
    "menu.edit.insert_file": "Lisää tiedosto…",
    # Search
    "menu.search.find": "Etsi…", "menu.search.replace": "Korvaa…",
    "menu.search.find_next": "Etsi seuraava", "menu.search.find_prev": "Etsi edellinen",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Korosta",
    "menu.search.emphasize.selection": "Valinta", "menu.search.emphasize.word": "Sana tai lause…",
    "menu.search.emphasize.regex": "Säännöllinen lauseke…", "menu.search.emphasize.clear": "Tyhjennä",
    # View
    "menu.view.highlighting": "Syntaksin korostus", "menu.view.paragraph_marks": "Kappalemerkit",
    "menu.view.whitespace": "Tyhjemerkit", "menu.view.line_numbers": "Rivinumerot",
    "menu.view.word_wrap": "Rivitys", "menu.view.wrap_hang": "Riippuva sisennys ({n})…",
    "menu.view.preview_pane": "Esikatseluruutu", "menu.view.shell_pane": "Komentotulkkiruutu",
    "menu.view.shell_output": "Komentotulkin tuloste -välilehti", "menu.view.split": "Jaa ruutu",
    "menu.view.split.side_by_side": "Jaa vierekkäin", "menu.view.split.top_bottom": "Jaa ylä- ja alaosaan",
    "menu.view.split.focus_next": "Aktivoi seuraava ruutu", "menu.view.split.swap": "Vaihda ruudut keskenään",
    "menu.view.split.zoom": "Suurenna aktiivinen ruutu (vaihto)", "menu.view.split.fullscreen": "Aktiivinen ruutu koko näytölle",
    "menu.view.split.remove": "Poista jako", "menu.view.menu_bar": "Valikkorivi",
    "menu.view.title_bar": "Otsikkorivi", "menu.view.writers_view": "Kirjoitustila (poistu Esc)",
    "menu.view.cmdline": "Komentorivi", "menu.view.cmdline.popup": "Ponnahdusikkuna",
    "menu.view.cmdline.position": "Sijainti", "menu.view.cmdline.position.top": "Ylhäällä",
    "menu.view.cmdline.position.bottom": "Alhaalla", "menu.view.cmdline.position.hidden": "Piilotettu",
    # Document
    "menu.document.doctype": "Asiakirjatyyppi", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Automaattinen (tarkenteen mukaan)",
    "menu.document.encoding": "Merkistökoodaus", "menu.document.encoding.display_as": "Näytä muodossa",
    "menu.document.encoding.convert_to": "Muunna muotoon (uusi tiedosto)", "menu.document.line_endings": "Rivinvaihdot",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Vain luku", "menu.document.word_count": "Sanamäärä",
    "menu.document.char_count": "Merkkimäärä", "menu.document.properties": "Ominaisuudet…",
    # Tools
    "menu.tools.run_script": "Suorita komentosarja…", "menu.tools.run_script_bg": "Suorita komentosarja taustalla…",
    "menu.tools.run_here": "Suorita kohdistimen alla oleva koodi", "menu.tools.shell_command": "Komentotulkin komento…",
    "menu.tools.jobs": "Taustatehtävät", "menu.tools.lang": "Kielentarkistus",
    "menu.tools.lang.check": "Tarkista asiakirja", "menu.tools.lang.spelling": "Oikeinkirjoituksen alleviivaus",
    "menu.tools.lang.grammar": "Kieliopin alleviivaus", "menu.tools.lang.spell_next": "Seuraava kirjoitusvirhe",
    "menu.tools.lang.spell_prev": "Edellinen kirjoitusvirhe", "menu.tools.lang.grammar_next": "Seuraava kielioppivirhe",
    "menu.tools.lang.grammar_prev": "Edellinen kielioppivirhe", "menu.tools.lang.fix": "Korjaa kohdistimen kohdalla oleva ongelma…",
    "menu.tools.lang.learn": "Opeta valitut sanat", "menu.tools.lang.dialect": "Murre",
    "menu.tools.lang.dialect.american": "Amerikanenglanti", "menu.tools.lang.dialect.british": "Britannianenglanti",
    "menu.tools.lang.dialect.canadian": "Kanadanenglanti", "menu.tools.lang.dialect.australian": "Australianenglanti",
    "menu.tools.lang.dialect.indian": "Intianenglanti", "menu.tools.lang.spell_style": "Oikeinkirjoitustyyli",
    "menu.tools.lang.grammar_style": "Kielioppityyli", "menu.tools.email": "Sähköposti",
    "menu.tools.email.list": "Listaa postilaatikko", "menu.tools.email.save": "Tallenna viestit…",
    "menu.tools.email.delete": "Poista viestit…", "menu.tools.email.send": "Lähetä…",
    "menu.tools.email.attach": "Liitä tiedosto…", "menu.tools.email.clear_attach": "Tyhjennä liitteet",
    "menu.tools.email.verbose": "Yksityiskohtainen loki", "menu.tools.library": "Kirjasto",
    "menu.tools.library.load": "Lataa…", "menu.tools.library.reload": "Lataa uudelleen",
    "menu.tools.library.unload": "Poista kaikki muistista", "menu.tools.library.list": "Listaa rutiinit",
    "menu.tools.keyboard_chart": "Näppäimistökaavio",
    # Settings
    "menu.settings.appearance": "Ulkoasu", "menu.settings.appearance.colors": "Värit",
    "menu.settings.appearance.colors.editor_text": "Editorin teksti…", "menu.settings.appearance.colors.editor_bg": "Editorin tausta…",
    "menu.settings.appearance.colors.selection_text": "Valinnan teksti…", "menu.settings.appearance.colors.selection_bg": "Valinnan tausta…",
    "menu.settings.appearance.colors.caret": "Kohdistin…", "menu.settings.appearance.colors.separators": "Erottimet…",
    "menu.settings.appearance.colors.tab_bg": "Välilehtirivin tausta", "menu.settings.appearance.colors.tab_bg.active": "Aktiivinen välilehti…",
    "menu.settings.appearance.colors.tab_bg.hover": "Osoitettu välilehti…", "menu.settings.appearance.colors.tab_bg.inactive": "Ei-aktiivinen välilehti…",
    "menu.settings.appearance.colors.tab_close": "Välilehden sulkeminen ×…", "menu.settings.appearance.colors.ui_text": "Käyttöliittymän teksti…",
    "menu.settings.appearance.colors.ui_bg": "Käyttöliittymän tausta…", "menu.settings.appearance.colors.txt_text": "TXT-ikkunan teksti…",
    "menu.settings.appearance.colors.txt_bg": "TXT-ikkunan tausta…", "menu.settings.appearance.colors.dir_text": "Hakemistoikkunan teksti…",
    "menu.settings.appearance.colors.dir_bg": "Hakemistoikkunan tausta…", "menu.settings.appearance.fonts": "Fontit",
    "menu.settings.appearance.fonts.editor": "Editori…", "menu.settings.appearance.fonts.ui": "Käyttöliittymä…",
    "menu.settings.appearance.fonts.txt": "TXT-ikkuna…", "menu.settings.appearance.fonts.dir": "Hakemistoikkuna…",
    "menu.settings.appearance.fonts.preview_body": "Esikatselun / PDF:n leipäteksti…", "menu.settings.appearance.fonts.preview_code": "Esikatselun / PDF:n koodi…",
    "menu.settings.appearance.ui_zoom": "Käyttöliittymän zoomaus", "menu.settings.appearance.ui_zoom.in": "Lähennä",
    "menu.settings.appearance.ui_zoom.out": "Loitonna", "menu.settings.appearance.ui_zoom.reset": "Palauta (100 %)",
    "menu.settings.appearance.margins": "Marginaalit", "menu.settings.appearance.margins.wider": "Leveämmät",
    "menu.settings.appearance.margins.narrower": "Kapeammat", "menu.settings.appearance.margins.none": "Ei marginaaleja",
    "menu.settings.appearance.line_spacing": "Riviväli", "menu.settings.appearance.line_spacing.more": "Enemmän",
    "menu.settings.appearance.line_spacing.less": "Vähemmän", "menu.settings.appearance.reset_ui": "Palauta käyttöliittymän koko",
    "menu.settings.appearance.reset_dw": "Palauta hakemistoikkuna", "menu.settings.syntax": "Syntaksin korostus",
    "menu.settings.syntax.theme_colors": "Teeman värit", "menu.settings.syntax.token_kinds": "Token-tyypit…",
    "menu.settings.syntax.reset_theme": "Palauta teema", "menu.settings.syntax.auto": "Automaattinen korostus avattaessa",
    "menu.settings.editor": "Editorin toiminta", "menu.settings.editor.persistent_sel": "Pysyvä valinta",
    "menu.settings.editor.remember_geom": "Muista ikkunan koko ja sijainti", "menu.settings.editor.coding": "Koodausavut",
    "menu.settings.editor.coding.autoindent": "Uusien rivien automaattinen sisennys", "menu.settings.editor.coding.soft_tabs": "Pehmeät sarkaimet (lisää välilyöntejä)",
    "menu.settings.editor.coding.hang_indent": "Riippuva sisennys rivitetyille riveille", "menu.settings.editor.coding.match_brackets": "Korosta vastaavat sulkeet",
    "menu.settings.editor.coding.close_brackets": "Sulje sulkeet ja lainausmerkit automaattisesti", "menu.settings.editor.autosave": "Automaattinen tallennus",
    "menu.settings.editor.autosave.off": "Pois käytöstä", "menu.settings.editor.autosave.30s": "30 sekunnin välein",
    "menu.settings.editor.autosave.1m": "1 minuutin välein", "menu.settings.editor.autosave.2m": "2 minuutin välein",
    "menu.settings.editor.autosave.5m": "5 minuutin välein", "menu.settings.editor.tab_width": "Sarkaimen leveys",
    "menu.settings.editor.precision": "Numeerinen tarkkuus", "menu.settings.editor.precision.off": "Pois käytöstä (täysi)",
    "menu.settings.keyboard": "Näppäimistö", "menu.settings.keyboard.load": "Lataa…",
    "menu.settings.keyboard.save": "Tallenna…", "menu.settings.keyboard.reset": "Palauta sisäänrakennettuun näppäimistöön",
    "menu.settings.allow_shell": "Salli komentotulkin komennot", "menu.settings.save_settings": "Tallenna asetukset…",
    "menu.settings.load_settings": "Lataa asetukset…", "menu.settings.autosave_settings": "Tallenna asetukset automaattisesti",
    # Help
    "menu.help.reference": "Viiteopas", "menu.help.about": "Tietoja XyScriptistä",
    # Dialogs (wired)
    "button.close": "Sulje",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Etsi:",
    "dialog.grep.pattern_hint": "teksti (tai säännöllinen lauseke Säännöllinen lauseke -valinnalla)",
    "dialog.grep.folder": "Kansiossa:", "dialog.grep.folder_hint": "tyhjä = nykyinen kansio — tai  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Säännöllinen lauseke", "dialog.grep.ignore_case": "Ohita kirjainkoko",
    "dialog.grep.whole_word": "Koko sana", "dialog.grep.search": "Etsi",
    "dialog.run.title": "Suorita komento",
    "dialog.run.shell_off": "Komentotulkin komennot ovat pois käytöstä — ota ne käyttöön kohdasta Asetukset ⏵ Salli komentotulkin komennot.",
    "dialog.run.command": "Komento:", "dialog.run.command_hint": "komentotulkin komento",
    "dialog.run.background": "Suorita taustalla", "dialog.run.insert_output": "Lisää tuloste kohdistimeen",
    "dialog.run.run": "Suorita",
    "dialog.find.find_label": "Etsi:", "dialog.find.find_hint": "etsittävä teksti",
    "dialog.find.replace_label": "Korvaa:", "dialog.find.replace_hint": "korvaava teksti",
    "dialog.find.case": "Huomioi kirjainkoko", "dialog.find.whole_word": "Koko sana",
    "dialog.find.regex": "Säännöllinen lauseke", "dialog.find.in_selection": "Valinnan sisällä",
    "dialog.find.keep_case": "Säilytä kirjainkoko korvattaessa",
    "dialog.find.keep_case_hint": "Korvaa / Korvaa kaikki: mukauttaa kunkin korvauksen kirjainkoon korvattavan tekstin mukaan (pienaakkoset / Alkukirjain / SUURAAKKOSET).",
    "dialog.find.find_next": "Seuraava (F3)", "dialog.find.find_prev": "Edellinen (Vaihto+F3)",
    "dialog.find.replace": "Korvaa", "dialog.find.replace_all": "Korvaa kaikki",
    "dialog.about.title": "Tietoja XyScriptistä", "dialog.about.tagline": "Muokattava editori XyWrite-perinteessä.",
    # Messages (curated phase-2 batch — keep every {placeholder} intact)
    "msg.search.not_found": "ei löytynyt: {find}",
    "msg.open.not_found": "tiedostoa ei löytynyt: {path}",
    "msg.save.written": "tallennettu {name}",
    "msg.readonly": "asiakirja on vain luku -tilassa (RO avaa lukituksen)",
    "msg.grep.none": "GREP: ei osumia",
    "msg.grep.searching": "GREP: etsitään…",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  SUOMI (sample translation)
# =============================================================================
# A sample Finnish catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-fi.txt
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
        val = FI.get(key, "")
        if not val:
            missing.append(key)
        out.append(f"{key} = {val}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}; {len(missing)} untranslated key(s)" + (": " + ", ".join(missing) if missing else ""))
