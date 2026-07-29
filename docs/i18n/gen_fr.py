#!/usr/bin/env python3
"""Generate a sample FRENCH catalog from the English template (structure stays in sync)."""
import re, sys
SRC, DST = sys.argv[1], sys.argv[2]

FR = {
    # menu bar
    "menubar.file": "Fichier", "menubar.edit": "Édition", "menubar.search": "Rechercher",
    "menubar.view": "Affichage", "menubar.document": "Document", "menubar.tools": "Outils",
    "menubar.settings": "Paramètres", "menubar.help": "Aide",
    # File
    "menu.file.new": "Nouveau", "menu.file.open": "Ouvrir…",
    "menu.file.open_readonly": "Ouvrir en lecture seule…", "menu.file.revert": "Rétablir le fichier enregistré",
    "menu.file.save": "Enregistrer", "menu.file.save_as": "Enregistrer sous…",
    "menu.file.save_selection_as": "Enregistrer la sélection sous…", "menu.file.save_all": "Tout enregistrer",
    "menu.file.export_pdf": "Exporter en PDF…", "menu.file.close": "Fermer", "menu.file.quit": "Quitter",
    # Edit
    "menu.edit.undo": "Annuler", "menu.edit.redo": "Rétablir", "menu.edit.cut": "Couper",
    "menu.edit.copy": "Copier", "menu.edit.append_clipboard": "Ajouter au presse-papiers",
    "menu.edit.prepend_clipboard": "Insérer au début du presse-papiers", "menu.edit.paste": "Coller",
    "menu.edit.select_all": "Tout sélectionner", "menu.edit.delete_selection": "Supprimer la sélection",
    "menu.edit.copy_to_cursor": "Copier au curseur", "menu.edit.move_to_cursor": "Déplacer au curseur",
    "menu.edit.case": "Changer la casse", "menu.edit.case.upper": "MAJUSCULES",
    "menu.edit.case.lower": "minuscules", "menu.edit.case.swap": "Inverser la casse",
    "menu.edit.text_colour": "Couleur du texte…", "menu.edit.clear_text_colour": "Effacer la couleur du texte",
    "menu.edit.insert_file": "Insérer un fichier…",
    # Search
    "menu.search.find": "Rechercher…", "menu.search.replace": "Remplacer…",
    "menu.search.find_next": "Rechercher le suivant", "menu.search.find_prev": "Rechercher le précédent",
    "menu.search.grep": "Grep…", "menu.search.emphasize": "Mettre en évidence",
    "menu.search.emphasize.selection": "Sélection", "menu.search.emphasize.word": "Mot ou phrase…",
    "menu.search.emphasize.regex": "Expression régulière…", "menu.search.emphasize.clear": "Effacer",
    # View
    "menu.view.highlighting": "Coloration syntaxique", "menu.view.paragraph_marks": "Marques de paragraphe",
    "menu.view.whitespace": "Espaces", "menu.view.line_numbers": "Numéros de ligne",
    "menu.view.word_wrap": "Retour à la ligne automatique", "menu.view.wrap_hang": "Retrait suspendu ({n})…",
    "menu.view.preview_pane": "Volet d’aperçu", "menu.view.shell_pane": "Volet shell",
    "menu.view.shell_output": "Onglet de sortie shell", "menu.view.split": "Fractionner les volets",
    "menu.view.split.side_by_side": "Fractionner côte à côte", "menu.view.split.top_bottom": "Fractionner haut et bas",
    "menu.view.split.focus_next": "Activer le volet suivant", "menu.view.split.swap": "Échanger les deux volets",
    "menu.view.split.zoom": "Agrandir le volet actif (bascule)", "menu.view.split.fullscreen": "Volet actif en plein écran",
    "menu.view.split.remove": "Supprimer le fractionnement", "menu.view.menu_bar": "Barre de menus",
    "menu.view.title_bar": "Barre de titre", "menu.view.writers_view": "Mode rédaction (Échap pour quitter)",
    "menu.view.cmdline": "Ligne de commande", "menu.view.cmdline.popup": "Fenêtre contextuelle",
    "menu.view.cmdline.position": "Position", "menu.view.cmdline.position.top": "Haut",
    "menu.view.cmdline.position.bottom": "Bas", "menu.view.cmdline.position.hidden": "Masquée",
    # Document
    "menu.document.doctype": "Type de document", "menu.document.doctype.typst": "Typst",
    "menu.document.doctype.gfm": "GFM (Markdown)", "menu.document.doctype.auto": "Auto (par extension)",
    "menu.document.encoding": "Encodage du texte", "menu.document.encoding.display_as": "Afficher comme",
    "menu.document.encoding.convert_to": "Convertir en (nouveau fichier)", "menu.document.line_endings": "Fins de ligne",
    "menu.document.line_endings.lf": "LF (Unix)", "menu.document.line_endings.crlf": "CRLF (Windows)",
    "menu.document.read_only": "Lecture seule", "menu.document.word_count": "Nombre de mots",
    "menu.document.char_count": "Nombre de caractères", "menu.document.properties": "Propriétés…",
    # Tools
    "menu.tools.run_script": "Exécuter un script…", "menu.tools.run_script_bg": "Exécuter un script en arrière-plan…",
    "menu.tools.run_here": "Exécuter le code sous le curseur", "menu.tools.shell_command": "Commande shell…",
    "menu.tools.jobs": "Tâches en arrière-plan", "menu.tools.lang": "Vérification linguistique",
    "menu.tools.lang.check": "Vérifier le document", "menu.tools.lang.spelling": "Soulignement orthographique",
    "menu.tools.lang.grammar": "Soulignement grammatical", "menu.tools.lang.spell_next": "Problème d’orthographe suivant",
    "menu.tools.lang.spell_prev": "Problème d’orthographe précédent", "menu.tools.lang.grammar_next": "Problème de grammaire suivant",
    "menu.tools.lang.grammar_prev": "Problème de grammaire précédent", "menu.tools.lang.fix": "Corriger le problème au curseur…",
    "menu.tools.lang.learn": "Apprendre les mots sélectionnés", "menu.tools.lang.dialect": "Dialecte",
    "menu.tools.lang.dialect.american": "Américain", "menu.tools.lang.dialect.british": "Britannique",
    "menu.tools.lang.dialect.canadian": "Canadien", "menu.tools.lang.dialect.australian": "Australien",
    "menu.tools.lang.dialect.indian": "Indien", "menu.tools.lang.spell_style": "Style d’orthographe",
    "menu.tools.lang.grammar_style": "Style de grammaire", "menu.tools.email": "Courriel",
    "menu.tools.email.list": "Lister la boîte aux lettres", "menu.tools.email.save": "Enregistrer les messages…",
    "menu.tools.email.delete": "Supprimer les messages…", "menu.tools.email.send": "Envoyer…",
    "menu.tools.email.attach": "Joindre un fichier…", "menu.tools.email.clear_attach": "Effacer les pièces jointes",
    "menu.tools.email.verbose": "Journalisation détaillée", "menu.tools.library": "Bibliothèque",
    "menu.tools.library.load": "Charger…", "menu.tools.library.reload": "Recharger",
    "menu.tools.library.unload": "Tout décharger", "menu.tools.library.list": "Lister les routines",
    "menu.tools.keyboard_chart": "Tableau du clavier",
    # Settings
    "menu.settings.appearance": "Apparence", "menu.settings.appearance.colors": "Couleurs",
    "menu.settings.appearance.colors.editor_text": "Texte de l’éditeur…", "menu.settings.appearance.colors.editor_bg": "Arrière-plan de l’éditeur…",
    "menu.settings.appearance.colors.selection_text": "Texte de la sélection…", "menu.settings.appearance.colors.selection_bg": "Arrière-plan de la sélection…",
    "menu.settings.appearance.colors.caret": "Curseur…", "menu.settings.appearance.colors.separators": "Séparateurs…",
    "menu.settings.appearance.colors.tab_bg": "Arrière-plan de la barre d’onglets", "menu.settings.appearance.colors.tab_bg.active": "Onglet actif…",
    "menu.settings.appearance.colors.tab_bg.hover": "Onglet survolé…", "menu.settings.appearance.colors.tab_bg.inactive": "Onglet inactif…",
    "menu.settings.appearance.colors.tab_close": "Fermeture d’onglet ×…", "menu.settings.appearance.colors.ui_text": "Texte de l’interface…",
    "menu.settings.appearance.colors.ui_bg": "Arrière-plan de l’interface…", "menu.settings.appearance.colors.txt_text": "Texte de la fenêtre TXT…",
    "menu.settings.appearance.colors.txt_bg": "Arrière-plan de la fenêtre TXT…", "menu.settings.appearance.colors.dir_text": "Texte de la fenêtre de répertoire…",
    "menu.settings.appearance.colors.dir_bg": "Arrière-plan de la fenêtre de répertoire…", "menu.settings.appearance.fonts": "Polices",
    "menu.settings.appearance.fonts.editor": "Éditeur…", "menu.settings.appearance.fonts.ui": "Interface…",
    "menu.settings.appearance.fonts.txt": "Fenêtre TXT…", "menu.settings.appearance.fonts.dir": "Fenêtre de répertoire…",
    "menu.settings.appearance.fonts.preview_body": "Corps de l’aperçu / PDF…", "menu.settings.appearance.fonts.preview_code": "Code de l’aperçu / PDF…",
    "menu.settings.appearance.ui_zoom": "Zoom de l’interface", "menu.settings.appearance.ui_zoom.in": "Zoom avant",
    "menu.settings.appearance.ui_zoom.out": "Zoom arrière", "menu.settings.appearance.ui_zoom.reset": "Réinitialiser (100 %)",
    "menu.settings.appearance.margins": "Marges", "menu.settings.appearance.margins.wider": "Plus larges",
    "menu.settings.appearance.margins.narrower": "Plus étroites", "menu.settings.appearance.margins.none": "Aucune",
    "menu.settings.appearance.line_spacing": "Interligne", "menu.settings.appearance.line_spacing.more": "Plus",
    "menu.settings.appearance.line_spacing.less": "Moins", "menu.settings.appearance.reset_ui": "Réinitialiser la taille de l’interface",
    "menu.settings.appearance.reset_dw": "Réinitialiser la fenêtre de répertoire", "menu.settings.syntax": "Coloration syntaxique",
    "menu.settings.syntax.theme_colors": "Couleurs du thème", "menu.settings.syntax.token_kinds": "Types de jetons…",
    "menu.settings.syntax.reset_theme": "Réinitialiser le thème", "menu.settings.syntax.auto": "Coloration automatique à l’ouverture",
    "menu.settings.editor": "Comportement de l’éditeur", "menu.settings.editor.persistent_sel": "Sélection persistante",
    "menu.settings.editor.remember_geom": "Mémoriser la taille et la position de la fenêtre", "menu.settings.editor.coding": "Aides à la saisie",
    "menu.settings.editor.coding.autoindent": "Indentation automatique des nouvelles lignes", "menu.settings.editor.coding.soft_tabs": "Tabulations douces (insérer des espaces)",
    "menu.settings.editor.coding.hang_indent": "Retrait suspendu pour les lignes renvoyées", "menu.settings.editor.coding.match_brackets": "Mettre en évidence les parenthèses correspondantes",
    "menu.settings.editor.coding.close_brackets": "Fermeture automatique des parenthèses et guillemets", "menu.settings.editor.autosave": "Enregistrement automatique",
    "menu.settings.editor.autosave.off": "Désactivé", "menu.settings.editor.autosave.30s": "Toutes les 30 s",
    "menu.settings.editor.autosave.1m": "Toutes les 1 min", "menu.settings.editor.autosave.2m": "Toutes les 2 min",
    "menu.settings.editor.autosave.5m": "Toutes les 5 min", "menu.settings.editor.tab_width": "Largeur de tabulation",
    "menu.settings.editor.precision": "Précision numérique", "menu.settings.editor.precision.off": "Désactivé (complet)",
    "menu.settings.keyboard": "Clavier", "menu.settings.keyboard.load": "Charger…",
    "menu.settings.keyboard.save": "Enregistrer…", "menu.settings.keyboard.reset": "Réinitialiser au clavier intégré",
    "menu.settings.allow_shell": "Autoriser les commandes shell", "menu.settings.save_settings": "Enregistrer les paramètres…",
    "menu.settings.load_settings": "Charger les paramètres…", "menu.settings.autosave_settings": "Enregistrement automatique des paramètres",
    # Help
    "menu.help.reference": "Référence", "menu.help.about": "À propos de XyScript",
    # Dialogs (wired)
    "button.close": "Fermer",
    "dialog.grep.title": "Grep", "dialog.grep.pattern": "Rechercher :",
    "dialog.grep.pattern_hint": "texte (ou une expression régulière avec Expression régulière)",
    "dialog.grep.folder": "Dans le dossier :", "dialog.grep.folder_hint": "vide = dossier courant — ou  src  ·  *.rs  ·  src/*.rs",
    "dialog.grep.regex": "Expression régulière", "dialog.grep.ignore_case": "Ignorer la casse",
    "dialog.grep.whole_word": "Mot entier", "dialog.grep.search": "Rechercher",
    "dialog.run.title": "Exécuter une commande",
    "dialog.run.shell_off": "Les commandes shell sont désactivées — activez-les dans Paramètres ⏵ Autoriser les commandes shell.",
    "dialog.run.command": "Commande :", "dialog.run.command_hint": "une commande shell",
    "dialog.run.background": "Exécuter en arrière-plan", "dialog.run.insert_output": "Insérer la sortie au curseur",
    "dialog.run.run": "Exécuter",
    "dialog.find.find_label": "Rechercher :", "dialog.find.find_hint": "texte à rechercher",
    "dialog.find.replace_label": "Remplacer :", "dialog.find.replace_hint": "remplacement",
    "dialog.find.case": "Respecter la casse", "dialog.find.whole_word": "Mot entier",
    "dialog.find.regex": "Expression régulière", "dialog.find.in_selection": "Dans la sélection",
    "dialog.find.keep_case": "Conserver la casse dans le remplacement",
    "dialog.find.keep_case_hint": "Remplacer / Remplacer tout : adapte la casse de chaque remplacement à celle du texte remplacé (minuscules / Capitalisé / MAJUSCULES).",
    "dialog.find.find_next": "Suivant (F3)", "dialog.find.find_prev": "Précédent (Maj+F3)",
    "dialog.find.replace": "Remplacer", "dialog.find.replace_all": "Remplacer tout",
    "dialog.about.title": "À propos de XyScript", "dialog.about.tagline": "Un éditeur configurable dans la tradition XyWrite.",
    # Messages (not wired yet — phase 2 — placeholders preserved)
    "msg.grep.summary": "{count} correspondances dans {files} fichiers", "msg.grep.none": "Aucune correspondance",
    "msg.grep.running": "Recherche… {count} correspondances jusqu’à présent", "msg.save.written": "Enregistré {path}",
    "msg.open.not_found": "Fichier introuvable : {path}", "msg.readonly": "Ce tampon est en lecture seule",
}

HEADER = """\
# =============================================================================
# XyScript UI strings  —  FRANÇAIS (sample translation)
# =============================================================================
# A sample French catalog, generated from xyscript-strings.template. Load it:
#   LANG <path>\\xyscript-strings-fr.txt
# Menu labels are translated (phase 1); dialog + message strings are translated
# too but not wired yet, so they won't show until phase 2. Any untranslated key
# falls back to English. {placeholders} kept intact.
# =============================================================================

"""

ENTRY = re.compile(r'^([A-Za-z][\w.]*)\s*=\s*#\s*(.*)$')
out = [HEADER]
started = False
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
        fr = FR.get(key, "")
        out.append(f"{key} = {fr}   # {comment}")
    else:
        out.append(line)
open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
missing = [k for k in ()]  # placeholder
print(f"wrote {DST}; translated {sum(1 for l in out if ' = ' in l and not l.strip().endswith('#'+l.split('#',1)[1] if '#' in l else False)) }")
